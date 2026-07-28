//! Optional on-disk cache for realised flake-app store executables.
//!
//! Warm hits spawn the cached `/nix/store/…` program directly (skipping
//! `nix run`) when fingerprints still match and the path remains valid.
//! Secret *values* are never persisted. Disabled with `NXR_STORE_EXE_CACHE=off`
//! (or `0` / `false` / `no`). See ADR-0153.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::plan_cache::PlanCacheSharedFingerprints;
use crate::{add_bytes_hashed, record_fs_metadata};

/// Environment variable disabling the store-exe cache (`off`, `0`, `false`, `no`).
pub const STORE_EXE_CACHE_ENV: &str = "NXR_STORE_EXE_CACHE";

/// On-disk schema version for store-exe cache entries.
pub const STORE_EXE_CACHE_SCHEMA_VERSION: u32 = 1;

/// Default TTL backstop (24 hours). `NXR_STORE_EXE_CACHE_TTL_SECS=0` disables TTL.
pub const DEFAULT_STORE_EXE_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Environment variable overriding store-exe cache TTL in seconds.
pub const STORE_EXE_CACHE_TTL_ENV: &str = "NXR_STORE_EXE_CACHE_TTL_SECS";

/// Inputs that identify a cached realised executable (hashed into the file key).
///
/// Forwarded app arguments are intentionally excluded: the store program is
/// independent of argv. Shell wraps are handled by callers (skip this cache).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoreExeCacheKeyMaterial {
    pub flake_ref: String,
    pub local_root: String,
    pub system: String,
    pub app_name: String,
    pub attr_path: String,
    pub nix_flags_digest: String,
    pub fingerprints: PlanCacheSharedFingerprints,
}

/// On-disk store-exe cache summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoreExeCacheStatus {
    pub path: String,
    pub entries: usize,
    pub total_bytes: u64,
}

/// Hit payload returned from [`lookup_store_exe`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreExeCacheHit {
    pub program: String,
    pub store_output: String,
    pub fingerprints: PlanCacheSharedFingerprints,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedStoreExe {
    schema_version: u32,
    key_digest: String,
    program: String,
    store_output: String,
    fingerprints: PlanCacheSharedFingerprints,
    recorded_at: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_CACHE_TTL_SECS: std::cell::RefCell<Option<Option<u64>>> = const { std::cell::RefCell::new(None) };
}

/// Whether the store-exe disk cache is enabled.
#[must_use]
pub fn store_exe_cache_enabled() -> bool {
    cache_enabled_for_env(std::env::var(STORE_EXE_CACHE_ENV).ok().as_deref())
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

/// Store-exe cache directory when the host provides a writable cache location.
#[must_use]
pub fn store_exe_cache_dir() -> Option<PathBuf> {
    cache_root()
}

/// BLAKE3 hex digest of [`StoreExeCacheKeyMaterial`].
#[must_use]
pub fn store_exe_cache_key_digest(material: &StoreExeCacheKeyMaterial) -> String {
    let mut hasher = Hasher::new();
    hash_str(&mut hasher, "store-exe-v1");
    hash_str(&mut hasher, &material.flake_ref);
    hash_str(&mut hasher, &material.local_root);
    hash_str(&mut hasher, &material.system);
    hash_str(&mut hasher, &material.app_name);
    hash_str(&mut hasher, &material.attr_path);
    hash_str(&mut hasher, &material.nix_flags_digest);
    hash_str(&mut hasher, &material.fingerprints.nix_tree_fingerprint);
    hash_str(
        &mut hasher,
        &material.fingerprints.discovery_inputs_fingerprint,
    );
    hash_opt_str(
        &mut hasher,
        material.fingerprints.flake_lock_digest.as_deref(),
    );
    hash_str(&mut hasher, &material.fingerprints.nix_path);
    hash_str(&mut hasher, &material.fingerprints.nix_version);
    hash_opt_str(
        &mut hasher,
        material.fingerprints.nix_file_identity.as_deref(),
    );
    let digest = hasher.finalize();
    add_bytes_hashed(digest.as_bytes().len() as u64);
    digest.to_hex().to_string()
}

/// Look up a realised store executable by key digest.
///
/// Returns `None` on miss, corruption, schema mismatch, TTL expiry, fingerprint
/// mismatch, or when the cache is disabled / unavailable. Callers must still
/// verify the program path exists before spawning.
#[must_use]
pub fn lookup_store_exe(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> Option<StoreExeCacheHit> {
    if !store_exe_cache_enabled() {
        return None;
    }
    load_cached_exe(key_digest, expected).ok().flatten()
}

/// Store a realised executable under `key_digest`.
///
/// No-ops when the cache is disabled or unavailable.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory or entry cannot be written.
pub fn store_store_exe(
    key_digest: &str,
    program: &str,
    store_output: &str,
    fingerprints: PlanCacheSharedFingerprints,
) -> io::Result<()> {
    if !store_exe_cache_enabled() {
        return Ok(());
    }
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let entry = CachedStoreExe {
        schema_version: STORE_EXE_CACHE_SCHEMA_VERSION,
        key_digest: key_digest.to_owned(),
        program: program.to_owned(),
        store_output: store_output.to_owned(),
        fingerprints,
        recorded_at: unix_now_secs(),
    };
    let payload = serde_json::to_vec_pretty(&entry)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let tmp = path.with_extension("tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        file.lock_exclusive()?;
        file.write_all(&payload)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Remove all store-exe cache entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn clear_store_exe_cache() -> io::Result<usize> {
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

/// Summarize the store-exe cache directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn store_exe_cache_status() -> io::Result<StoreExeCacheStatus> {
    let Some(root) = cache_root() else {
        return Ok(StoreExeCacheStatus {
            path: String::new(),
            entries: 0,
            total_bytes: 0,
        });
    };

    if !root.is_dir() {
        return Ok(StoreExeCacheStatus {
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
            record_fs_metadata();
            total_bytes += entry.metadata()?.len();
        }
    }

    Ok(StoreExeCacheStatus {
        path: root.display().to_string(),
        entries,
        total_bytes,
    })
}

/// Whether `program` looks like a usable store executable for direct spawn.
#[must_use]
pub fn store_exe_path_usable(program: &Path) -> bool {
    record_fs_metadata();
    let Ok(metadata) = fs::metadata(program) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Derive the `/nix/store/<hash-name>` output root from an app `program` path.
#[must_use]
pub fn store_output_root_for_program(program: &str) -> Option<String> {
    let path = program.trim();
    let rest = path.strip_prefix("/nix/store/")?;
    let (hash_name, _) = rest.split_once('/')?;
    if hash_name.is_empty() {
        return None;
    }
    Some(format!("/nix/store/{hash_name}"))
}

fn load_cached_exe(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> io::Result<Option<StoreExeCacheHit>> {
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(None);
    };
    record_fs_metadata();
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let cached: CachedStoreExe = match serde_json::from_str(&contents) {
        Ok(cached) => cached,
        Err(_) => return Ok(None),
    };
    if cached.schema_version != STORE_EXE_CACHE_SCHEMA_VERSION {
        return Ok(None);
    }
    if cached.key_digest != key_digest {
        return Ok(None);
    }
    if &cached.fingerprints != expected {
        return Ok(None);
    }
    if let Some(ttl) = cache_ttl_secs()
        && unix_now_secs().saturating_sub(cached.recorded_at) > ttl
    {
        return Ok(None);
    }
    if cached.program.trim().is_empty() || cached.store_output.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(StoreExeCacheHit {
        program: cached.program,
        store_output: cached.store_output,
        fingerprints: cached.fingerprints,
    }))
}

fn cache_file_path(key_digest: &str) -> Option<PathBuf> {
    let root = cache_root()?;
    Some(root.join(format!("{key_digest}.json")))
}

fn cache_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_CACHE_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("store-exe"))
}

fn cache_ttl_secs() -> Option<u64> {
    #[cfg(test)]
    if let Some(override_ttl) = TEST_CACHE_TTL_SECS.with(|cell| *cell.borrow()) {
        return override_ttl;
    }

    match std::env::var(STORE_EXE_CACHE_TTL_ENV) {
        Ok(raw) => {
            let Ok(secs) = raw.parse::<u64>() else {
                return Some(DEFAULT_STORE_EXE_CACHE_TTL_SECS);
            };
            if secs == 0 { None } else { Some(secs) }
        }
        Err(_) => Some(DEFAULT_STORE_EXE_CACHE_TTL_SECS),
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn hash_str(hasher: &mut Hasher, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(&[0]);
}

fn hash_opt_str(hasher: &mut Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_str(hasher, value);
        }
        None => {
            hasher.update(&[0]);
            hasher.update(&[0]);
        }
    }
}

#[cfg(test)]
pub(crate) fn test_with_cache_dir<T>(root: PathBuf, f: impl FnOnce() -> T) -> T {
    TEST_CACHE_ROOT.with(|cell| {
        *cell.borrow_mut() = Some(root);
    });
    let result = f();
    TEST_CACHE_ROOT.with(|cell| {
        *cell.borrow_mut() = None;
    });
    result
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::plan_cache::PlanCacheSharedFingerprints;

    fn sample_fingerprints() -> PlanCacheSharedFingerprints {
        PlanCacheSharedFingerprints {
            nix_tree_fingerprint: "tree".to_owned(),
            discovery_inputs_fingerprint: "inputs".to_owned(),
            flake_lock_digest: Some("lock".to_owned()),
            nix_path: "/nix/bin/nix".to_owned(),
            nix_version: "2.34.0".to_owned(),
            nix_file_identity: Some("1:2:3".to_owned()),
        }
    }

    fn sample_key(fingerprints: PlanCacheSharedFingerprints) -> StoreExeCacheKeyMaterial {
        StoreExeCacheKeyMaterial {
            flake_ref: "/proj".to_owned(),
            local_root: "/proj".to_owned(),
            system: "local".to_owned(),
            app_name: "hello".to_owned(),
            attr_path: "apps.local.hello".to_owned(),
            nix_flags_digest: "flags".to_owned(),
            fingerprints,
        }
    }

    #[test]
    fn hit_miss_and_clear() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("store-exe");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_cache_dir(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = store_exe_cache_key_digest(&sample_key(fingerprints.clone()));
            assert!(lookup_store_exe(&key, &fingerprints).is_none());

            store_store_exe(
                &key,
                "/nix/store/abc-hello/bin/hello",
                "/nix/store/abc-hello",
                fingerprints.clone(),
            )
            .expect("store");

            let hit = lookup_store_exe(&key, &fingerprints).expect("hit");
            assert_eq!(hit.program, "/nix/store/abc-hello/bin/hello");
            assert_eq!(hit.store_output, "/nix/store/abc-hello");

            let mut stale = fingerprints.clone();
            stale.nix_tree_fingerprint = "changed".to_owned();
            assert!(lookup_store_exe(&key, &stale).is_none());

            assert_eq!(clear_store_exe_cache().expect("clear"), 1);
            assert!(lookup_store_exe(&key, &fingerprints).is_none());
        });
    }

    #[test]
    fn status_reports_entries() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("store-exe");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_cache_dir(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = store_exe_cache_key_digest(&sample_key(fingerprints.clone()));
            store_store_exe(
                &key,
                "/nix/store/abc-hello/bin/hello",
                "/nix/store/abc-hello",
                fingerprints,
            )
            .expect("store");
            let status = store_exe_cache_status().expect("status");
            assert_eq!(status.entries, 1);
            assert!(status.total_bytes > 0);
            assert!(!status.path.is_empty());
        });
    }

    #[test]
    fn kill_switch_parses_off_values() {
        assert!(cache_enabled_for_env(None));
        assert!(cache_enabled_for_env(Some("1")));
        assert!(!cache_enabled_for_env(Some("off")));
        assert!(!cache_enabled_for_env(Some("0")));
        assert!(!cache_enabled_for_env(Some("false")));
        assert!(!cache_enabled_for_env(Some("NO")));
    }

    #[test]
    fn store_output_root_parses_program_paths() {
        assert_eq!(
            store_output_root_for_program("/nix/store/abc-hello/bin/hello").as_deref(),
            Some("/nix/store/abc-hello")
        );
        assert_eq!(
            store_output_root_for_program("/nix/store/abc-hello/bin/nested/tool").as_deref(),
            Some("/nix/store/abc-hello")
        );
        assert!(store_output_root_for_program("/usr/bin/hello").is_none());
        assert!(store_output_root_for_program("/nix/store/").is_none());
    }

    #[test]
    fn key_independent_of_unrelated_fields_still_changes_with_app() {
        let fingerprints = sample_fingerprints();
        let a = store_exe_cache_key_digest(&sample_key(fingerprints.clone()));
        let mut other = sample_key(fingerprints);
        other.app_name = "other".to_owned();
        other.attr_path = "apps.local.other".to_owned();
        let b = store_exe_cache_key_digest(&other);
        assert_ne!(a, b);
    }
}
