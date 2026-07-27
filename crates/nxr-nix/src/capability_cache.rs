//! Persistent cache for Nix system and capability detection.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use camino::Utf8Path;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::NixError;
use crate::capabilities::{
    CapabilityEvidence, CapabilityProvenance, NixCapabilities, detect_capabilities_with_evidence,
    detect_system, probe_config_json,
};

/// Environment variable disabling the capability cache (`off`, `0`, or `false`).
pub const CAPABILITY_CACHE_ENV: &str = "NXR_CAPABILITY_CACHE";

/// Environment variable overriding capability cache TTL in seconds.
///
/// Unset → default [`DEFAULT_CACHE_TTL_SECS`] (7 days). `0` disables the TTL backstop.
pub const CAPABILITY_CACHE_TTL_ENV: &str = "NXR_CAPABILITY_CACHE_TTL_SECS";

/// Default capability cache TTL (7 days).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// On-disk schema for capability cache entries.
///
/// v2 adds [`CachedEntry::config_digest`] so effective Nix configuration changes
/// (env and `nix config show`) invalidate warm hits under the same binary.
const CACHE_SCHEMA_VERSION: u32 = 2;

/// Environment variables that reshape effective Nix configuration without
/// changing the `nix` executable identity.
const CONFIG_ENV_KEYS: &[&str] = &["NIX_CONFIG", "NIX_USER_CONF_FILES", "NIX_CONF_DIR"];

/// Detected Nix environment (system + negotiated capabilities).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NixEnvironment {
    pub system: String,
    pub capabilities: NixCapabilities,
    pub provenance: CapabilityProvenance,
}

/// On-disk capability cache summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityCacheStatus {
    pub path: String,
    pub entries: usize,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FileIdentity {
    size: u64,
    modified_secs: u64,
    modified_nanos: u32,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct StoredNixIdentity {
    canonical_path: String,
    file: FileIdentity,
    version_banner: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableIdentity {
    canonical_path: String,
    file: FileIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedEntry {
    schema_version: u32,
    identity: StoredNixIdentity,
    /// Digest of effective Nix configuration at store time (see [`effective_config_digest`]).
    config_digest: String,
    current_system: String,
    capabilities: NixCapabilities,
    evidence: Vec<CapabilityEvidence>,
    recorded_at: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Whether the persistent capability cache is enabled.
#[must_use]
pub fn capability_cache_enabled() -> bool {
    cache_enabled_for_env(std::env::var(CAPABILITY_CACHE_ENV).ok().as_deref())
}

fn cache_enabled_for_env(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Capability cache directory when the host provides a writable cache location.
#[must_use]
pub fn capability_cache_dir() -> Option<PathBuf> {
    cache_root()
}

/// Detect the current system and capabilities, using the on-disk cache when valid.
///
/// Warm hits still probe `nix config show` once to compute
/// [`effective_config_digest`], then skip version/help/system when the digest
/// matches. That keeps config-derived flags (`flakes_enabled`, …) correct when
/// `NIX_CONFIG` or user/daemon Nix configuration changes under the same binary.
///
/// # Errors
///
/// Returns [`NixError`] when detection or cache I/O fails fatally.
pub fn detect_nix_environment(nix: &Utf8Path, refresh: bool) -> Result<NixEnvironment, NixError> {
    let executable = executable_identity(nix)?;
    let config_digest = effective_config_digest(nix);

    if capability_cache_enabled()
        && !refresh
        && let Some(entry) = load_cached_entry(&executable, &config_digest)
    {
        return Ok(NixEnvironment {
            system: entry.current_system,
            capabilities: entry.capabilities,
            provenance: CapabilityProvenance {
                from_cache: true,
                evidence: vec![CapabilityEvidence::Cache, CapabilityEvidence::Config],
            },
        });
    }

    let system = detect_system(nix)?;
    let (capabilities, evidence) = detect_capabilities_with_evidence(nix)?;
    let version_banner = capabilities.version.to_string();

    if capability_cache_enabled() {
        let identity = StoredNixIdentity {
            canonical_path: executable.canonical_path.clone(),
            file: executable.file.clone(),
            version_banner,
        };
        let entry = CachedEntry {
            schema_version: CACHE_SCHEMA_VERSION,
            identity,
            config_digest: config_digest.clone(),
            current_system: system.clone(),
            capabilities: capabilities.clone(),
            evidence: evidence.clone(),
            recorded_at: unix_now_secs(),
        };
        let _ = store_cached_entry(&executable, &config_digest, &entry);
    }

    Ok(NixEnvironment {
        system,
        capabilities,
        provenance: CapabilityProvenance {
            from_cache: false,
            evidence,
        },
    })
}

/// Remove all capability cache entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn clear_capability_cache() -> io::Result<usize> {
    let Some(root) = cache_root() else {
        return Ok(0);
    };
    if !root.is_dir() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "json" || ext == "tmp")
        {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Summarize the capability cache directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn capability_cache_status() -> io::Result<CapabilityCacheStatus> {
    let Some(root) = cache_root() else {
        return Ok(CapabilityCacheStatus {
            path: String::new(),
            entries: 0,
            total_bytes: 0,
        });
    };

    if !root.is_dir() {
        return Ok(CapabilityCacheStatus {
            path: root.display().to_string(),
            entries: 0,
            total_bytes: 0,
        });
    }

    let mut entries = 0usize;
    let mut total_bytes = 0u64;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            entries += 1;
            total_bytes += entry.metadata()?.len();
        }
    }

    Ok(CapabilityCacheStatus {
        path: root.display().to_string(),
        entries,
        total_bytes,
    })
}

fn executable_identity(nix: &Utf8Path) -> Result<ExecutableIdentity, NixError> {
    let canonical_path = nix
        .canonicalize_utf8()
        .unwrap_or_else(|_| nix.to_path_buf());
    let metadata =
        fs::metadata(canonical_path.as_std_path()).map_err(|source| NixError::SpawnFailed {
            nix: canonical_path.clone(),
            source,
        })?;
    Ok(ExecutableIdentity {
        canonical_path: canonical_path.into_string(),
        file: file_identity(&metadata),
    })
}

fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    let modified = metadata.modified().ok().and_then(system_time_to_parts);
    let (modified_secs, modified_nanos) = modified.unwrap_or((0, 0));
    FileIdentity {
        size: metadata.len(),
        modified_secs,
        modified_nanos,
        #[cfg(unix)]
        dev: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        ino: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    }
}

fn system_time_to_parts(time: SystemTime) -> Option<(u64, u32)> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some((duration.as_secs(), duration.subsec_nanos()))
}

fn load_cached_entry(executable: &ExecutableIdentity, config_digest: &str) -> Option<CachedEntry> {
    let path = cache_file_path(executable, config_digest)?;
    if !path.is_file() {
        return None;
    }

    let contents = fs::read_to_string(&path).ok()?;
    let cached: CachedEntry = serde_json::from_str(&contents).ok()?;

    if cached.schema_version != CACHE_SCHEMA_VERSION {
        return None;
    }
    if cached.identity.canonical_path != executable.canonical_path {
        return None;
    }
    if cached.identity.file != executable.file {
        return None;
    }
    if cached.config_digest != config_digest {
        return None;
    }
    if let Some(ttl) = cache_ttl_secs()
        && unix_now_secs().saturating_sub(cached.recorded_at) > ttl
    {
        return None;
    }

    Some(cached)
}

fn store_cached_entry(
    executable: &ExecutableIdentity,
    config_digest: &str,
    entry: &CachedEntry,
) -> io::Result<()> {
    let path = cache_file_path(executable, config_digest)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized = serde_json::to_vec_pretty(entry)?;
    let lock_path = path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let tmp_name = format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache"),
        std::process::id()
    );
    let tmp_path = path.with_file_name(tmp_name);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(&serialized)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

/// Digest of effective Nix configuration used for capability-cache validity.
///
/// Includes config-shaping environment variables and the JSON from
/// `nix config show` / `show-config` when available.
fn effective_config_digest(nix: &Utf8Path) -> String {
    config_digest_from_parts(
        |key| std::env::var(key).ok(),
        probe_config_json(nix).as_deref(),
    )
}

fn config_digest_from_parts(
    env_value: impl Fn(&str) -> Option<String>,
    config_json: Option<&str>,
) -> String {
    let mut hasher = Hasher::new();
    for key in CONFIG_ENV_KEYS {
        hasher.update(key.as_bytes());
        hasher.update(&[0]);
        match env_value(key) {
            Some(value) => {
                hasher.update(value.as_bytes());
            }
            None => {
                hasher.update(b"<unset>");
            }
        }
        hasher.update(&[0]);
    }
    match config_json {
        Some(json) => {
            hasher.update(b"config-json");
            hasher.update(&[0]);
            hasher.update(json.as_bytes());
        }
        None => {
            hasher.update(b"config-json-absent");
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn cache_file_path(executable: &ExecutableIdentity, config_digest: &str) -> Option<PathBuf> {
    let root = cache_root()?;
    let mut hasher = Hasher::new();
    hasher.update(executable.canonical_path.as_bytes());
    hasher.update(&[0]);
    hasher.update(&executable.file.size.to_le_bytes());
    hasher.update(&executable.file.modified_secs.to_le_bytes());
    hasher.update(&executable.file.modified_nanos.to_le_bytes());
    #[cfg(unix)]
    {
        hasher.update(&executable.file.dev.to_le_bytes());
        hasher.update(&executable.file.ino.to_le_bytes());
    }
    hasher.update(&[0]);
    hasher.update(config_digest.as_bytes());
    Some(root.join(format!("{}.json", hasher.finalize().to_hex())))
}

fn cache_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_CACHE_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("capabilities"))
}

fn cache_ttl_secs() -> Option<u64> {
    match std::env::var(CAPABILITY_CACHE_TTL_ENV) {
        Ok(raw) => raw.parse().ok(),
        Err(_) => Some(DEFAULT_CACHE_TTL_SECS),
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    use super::{
        CAPABILITY_CACHE_ENV, CachedEntry, StoredNixIdentity, TEST_CACHE_ROOT,
        cache_enabled_for_env, clear_capability_cache, detect_nix_environment,
        effective_config_digest, executable_identity, load_cached_entry, store_cached_entry,
    };
    use crate::capabilities::{
        NixCapabilities, TESTED_NIX_SUPPORT_FLOOR, detect_capabilities_with_evidence,
    };

    fn with_cache_dir<T>(temp: &TempDir, f: impl FnOnce() -> T) -> T {
        let cache_home = temp.path().join("capabilities");
        fs::create_dir_all(&cache_home).expect("create cache dir");
        TEST_CACHE_ROOT.with(|cell| {
            *cell.borrow_mut() = Some(cache_home);
        });
        let result = f();
        TEST_CACHE_ROOT.with(|cell| {
            *cell.borrow_mut() = None;
        });
        result
    }

    fn sample_executable(path: &Utf8PathBuf) -> super::ExecutableIdentity {
        executable_identity(path).expect("identity")
    }

    fn sample_entry(executable: &super::ExecutableIdentity, config_digest: &str) -> CachedEntry {
        CachedEntry {
            schema_version: super::CACHE_SCHEMA_VERSION,
            identity: StoredNixIdentity {
                canonical_path: executable.canonical_path.clone(),
                file: executable.file.clone(),
                version_banner: TESTED_NIX_SUPPORT_FLOOR.to_string(),
            },
            config_digest: config_digest.to_owned(),
            current_system: "aarch64-darwin".to_owned(),
            capabilities: NixCapabilities::all_supported_for_tests(TESTED_NIX_SUPPORT_FLOOR),
            evidence: vec![crate::capabilities::CapabilityEvidence::Config],
            recorded_at: super::unix_now_secs(),
        }
    }

    #[test]
    fn capability_cache_disabled_by_env() {
        assert!(!cache_enabled_for_env(Some("off")));
        assert!(!cache_enabled_for_env(Some("0")));
        assert!(!cache_enabled_for_env(Some("false")));
        assert!(cache_enabled_for_env(None));
        assert!(cache_enabled_for_env(Some("on")));
        let _ = CAPABILITY_CACHE_ENV;
    }

    #[test]
    fn cache_hit_returns_without_redetecting_system() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            store_cached_entry(&executable, &digest, &sample_entry(&executable, &digest))
                .expect("store");

            let env = detect_nix_environment(&nix, false).expect("detect");
            assert!(env.provenance.from_cache);
            assert_eq!(
                env.system, "aarch64-darwin",
                "cached system should be returned"
            );
        });
    }

    #[test]
    fn cache_miss_stores_entry() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let env = detect_nix_environment(&nix, false).expect("detect");
            assert!(!env.provenance.from_cache);

            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            let cached = load_cached_entry(&executable, &digest).expect("cache entry");
            assert_eq!(cached.current_system, env.system);
            assert_eq!(cached.capabilities.version, env.capabilities.version);
            assert_eq!(cached.config_digest, digest);
        });
    }

    #[test]
    fn executable_change_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            let mut stale = sample_entry(&executable, &digest);
            stale.identity.file.size = executable.file.size.saturating_add(1);
            store_cached_entry(&executable, &digest, &stale).expect("store stale");

            assert!(
                load_cached_entry(&executable, &digest).is_none(),
                "stale file identity should miss"
            );
        });
    }

    #[test]
    fn nix_config_env_change_misses_cache() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            store_cached_entry(&executable, &digest, &sample_entry(&executable, &digest))
                .expect("store");

            let changed = super::config_digest_from_parts(
                |key| {
                    if key == "NIX_CONFIG" {
                        Some("experimental-features = nix-command\nwarn-dirty = false".to_owned())
                    } else {
                        std::env::var(key).ok()
                    }
                },
                Some(r#"{"experimental-features":{"value":["nix-command"]}}"#),
            );
            assert_ne!(
                digest, changed,
                "effective config should reshape the digest"
            );
            assert!(
                load_cached_entry(&executable, &changed).is_none(),
                "digest change must miss the prior entry"
            );
        });
    }

    #[test]
    fn config_digest_from_parts_stable_for_same_inputs() {
        let left = super::config_digest_from_parts(|_| None, Some(r#"{"a":1}"#));
        let right = super::config_digest_from_parts(|_| None, Some(r#"{"a":1}"#));
        assert_eq!(left, right);
        let other = super::config_digest_from_parts(|_| None, Some(r#"{"a":2}"#));
        assert_ne!(left, other);
    }
    #[test]
    fn refresh_bypasses_cache() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            store_cached_entry(&executable, &digest, &sample_entry(&executable, &digest))
                .expect("seed");

            let env = detect_nix_environment(&nix, true).expect("refresh detect");
            assert!(!env.provenance.from_cache);
        });
    }

    #[test]
    fn clear_removes_entries() {
        let temp = TempDir::new().expect("tempdir");
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        with_cache_dir(&temp, || {
            let executable = sample_executable(&nix);
            let digest = effective_config_digest(&nix);
            store_cached_entry(&executable, &digest, &sample_entry(&executable, &digest))
                .expect("seed");
            let removed = clear_capability_cache().expect("clear");
            assert_eq!(removed, 1);
            assert!(load_cached_entry(&executable, &digest).is_none());
        });
    }

    #[test]
    fn known_version_skips_help_probes_when_config_present() {
        let nix = which::which("nix")
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok());
        let Some(nix) = nix else {
            eprintln!("skipping: nix not on PATH");
            return;
        };

        let (_, evidence) = detect_capabilities_with_evidence(&nix).expect("detect");
        if evidence.contains(&crate::capabilities::CapabilityEvidence::Config) {
            assert!(
                !evidence.contains(&crate::capabilities::CapabilityEvidence::HelpProbe),
                "help probes should be skipped when config is available: {evidence:?}"
            );
        }
    }
}
