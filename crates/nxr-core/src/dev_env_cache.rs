//! Optional on-disk cache for materialized development-environment snapshots.
//!
//! Stores normalized process-env snapshots keyed by flake + shell + Nix identity
//! fingerprints. Secret *values* are never persisted; snapshots with non-placeholder
//! secret variable fields are rejected. **Opt-in** via `NXR_DEV_ENV_CACHE=on` (or
//! `1` / `true` / `yes`); unset or any other value leaves the cache disabled.
//! See ADR-0171.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::plan_cache::PlanCacheSharedFingerprints;
use crate::{
    add_bytes_hashed, prune_timed_json_cache, record_fs_metadata, remove_timed_json_entry,
    summarize_timed_json_cache,
};

/// Environment variable enabling the dev-environment snapshot cache (`on`, `1`, `true`, `yes`).
pub const DEV_ENV_CACHE_ENV: &str = "NXR_DEV_ENV_CACHE";

/// On-disk schema version for dev-environment cache entries.
pub const DEV_ENV_CACHE_SCHEMA_VERSION: u32 = 1;

/// Protocol version for normalized [`DevEnvironmentSnapshot`] payloads.
pub const DEV_ENV_PROTOCOL_VERSION: u32 = 1;

/// Placeholder written into secret variable values (never a real secret).
pub const DEV_ENV_SECRET_RUNTIME_PLACEHOLDER: &str = "<runtime>";

/// Default TTL backstop (24 hours). `NXR_DEV_ENV_CACHE_TTL_SECS=0` disables TTL.
pub const DEFAULT_DEV_ENV_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Environment variable overriding dev-environment cache TTL in seconds.
pub const DEV_ENV_CACHE_TTL_ENV: &str = "NXR_DEV_ENV_CACHE_TTL_SECS";

/// Compact Nix executable identity carried in snapshots (not secret contents).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevEnvironmentNixIdentity {
    pub nix_path: String,
    pub nix_version: String,
    /// Compact executable identity (`size:mtime_secs:mtime_nanos[:dev:ino]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nix_file_identity: Option<String>,
    pub nix_tree_fingerprint: String,
}

/// Secret variable slot on a snapshot (values must not be persisted).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevEnvSecretVariable {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// Normalized process-compatible development-environment snapshot (ADR-0171).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevEnvironmentSnapshot {
    pub flake_identity: String,
    pub system: String,
    pub shell: String,
    pub nix_identity: DevEnvironmentNixIdentity,
    pub variables: BTreeMap<String, String>,
    pub path_entries: Vec<String>,
    pub unsupported_features: Vec<String>,
    pub fingerprints: PlanCacheSharedFingerprints,
    pub protocol_version: u32,
    /// Secret variable slots; values must be empty or the runtime placeholder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_variables: Vec<DevEnvSecretVariable>,
}

/// Inputs that identify a cached dev-environment snapshot (hashed into the file key).
///
/// Excludes caller environment, script contents, snapshot variables/path entries, and
/// CWD (unless a future shell definition depends on it — default exclude).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DevEnvironmentCacheKeyMaterial {
    pub flake_identity: String,
    pub local_root: String,
    pub system: String,
    pub shell_name: String,
    pub environment_mode: String,
    pub nix_flags_digest: String,
    pub fingerprints: PlanCacheSharedFingerprints,
    pub protocol_version: u32,
}

/// On-disk dev-environment cache summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DevEnvironmentCacheStatus {
    pub path: String,
    pub enabled: bool,
    pub ttl_secs: Option<u64>,
    pub entries: usize,
    pub total_bytes: u64,
    pub oldest_age_secs: Option<u64>,
    pub newest_age_secs: Option<u64>,
}

/// Hit payload returned from [`lookup_dev_environment_snapshot`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevEnvironmentCacheHit {
    pub snapshot: DevEnvironmentSnapshot,
    pub fingerprints: PlanCacheSharedFingerprints,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedDevEnvironmentSnapshot {
    schema_version: u32,
    key_digest: String,
    snapshot: DevEnvironmentSnapshot,
    fingerprints: PlanCacheSharedFingerprints,
    recorded_at: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_CACHE_TTL_SECS: std::cell::RefCell<Option<Option<u64>>> = const { std::cell::RefCell::new(None) };
    static TEST_CACHE_ENABLED: std::cell::RefCell<Option<bool>> = const { std::cell::RefCell::new(None) };
}

/// Whether the dev-environment disk cache is enabled.
#[must_use]
pub fn dev_env_cache_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = TEST_CACHE_ENABLED.with(|cell| *cell.borrow()) {
        return enabled;
    }

    cache_enabled_for_env(std::env::var(DEV_ENV_CACHE_ENV).ok().as_deref())
}

fn cache_enabled_for_env(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "on" | "1" | "true" | "yes")
        }
        None => false,
    }
}

/// Dev-environment cache directory when the host provides a writable cache location.
#[must_use]
pub fn dev_env_cache_dir() -> Option<PathBuf> {
    cache_root()
}

/// BLAKE3 hex digest of [`DevEnvironmentCacheKeyMaterial`].
#[must_use]
pub fn dev_env_cache_key_digest(material: &DevEnvironmentCacheKeyMaterial) -> String {
    let mut hasher = Hasher::new();
    hash_str(&mut hasher, "dev-env-v1");
    hash_str(&mut hasher, &material.flake_identity);
    hash_str(&mut hasher, &material.local_root);
    hash_str(&mut hasher, &material.system);
    hash_str(&mut hasher, &material.shell_name);
    hash_str(&mut hasher, &material.environment_mode);
    hash_str(&mut hasher, &material.nix_flags_digest);
    hasher.update(&material.protocol_version.to_le_bytes());
    hasher.update(&[0]);
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
    hash_opt_str(
        &mut hasher,
        material.fingerprints.source_identity.as_deref(),
    );
    let digest = hasher.finalize();
    add_bytes_hashed(digest.as_bytes().len() as u64);
    digest.to_hex().to_string()
}

/// Look up a dev-environment snapshot by key digest.
///
/// Returns `None` on miss, corruption, schema mismatch, TTL expiry, fingerprint
/// mismatch, secret values, or when the cache is disabled / unavailable.
#[must_use]
pub fn lookup_dev_environment_snapshot(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> Option<DevEnvironmentCacheHit> {
    if !dev_env_cache_enabled() {
        return None;
    }
    load_cached_snapshot(key_digest, expected).ok().flatten()
}

/// Store a dev-environment snapshot under `key_digest`.
///
/// No-ops when the cache is disabled, unavailable, or the snapshot contains secret
/// values (non-placeholder [`DevEnvSecretVariable::value`] or matching entries in
/// [`DevEnvironmentSnapshot::variables`]).
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory or entry cannot be written.
pub fn store_dev_environment_snapshot(
    key_digest: &str,
    snapshot: &DevEnvironmentSnapshot,
    fingerprints: PlanCacheSharedFingerprints,
) -> io::Result<()> {
    if !dev_env_cache_enabled() {
        return Ok(());
    }
    if snapshot_contains_secret_values(snapshot) {
        return Ok(());
    }
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        restrict_cache_dir_permissions(parent)?;
    }

    let entry = CachedDevEnvironmentSnapshot {
        schema_version: DEV_ENV_CACHE_SCHEMA_VERSION,
        key_digest: key_digest.to_owned(),
        snapshot: snapshot.clone(),
        fingerprints,
        recorded_at: unix_now_secs(),
    };
    let payload = serde_json::to_vec_pretty(&entry)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let tmp = unique_cache_temp_path(&path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        file.lock_exclusive()?;
        file.write_all(&payload)?;
        file.sync_all()?;
        restrict_cache_file_permissions(&tmp)?;
    }
    fs::rename(&tmp, &path)?;
    restrict_cache_file_permissions(&path)?;
    Ok(())
}

/// Remove all dev-environment cache entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn clear_dev_env_cache() -> io::Result<usize> {
    invalidate_dev_env_cache(None)
}

/// Remove one or all dev-environment cache entries on disk.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn invalidate_dev_env_cache(key_digest: Option<&str>) -> io::Result<usize> {
    let Some(root) = cache_root() else {
        return Ok(0);
    };
    if !root.is_dir() {
        return Ok(0);
    }

    if let Some(key_digest) = key_digest {
        return remove_timed_json_entry(&root, key_digest).map(usize::from);
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

/// Prune TTL-expired dev-environment cache entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn gc_dev_env_cache() -> io::Result<usize> {
    let Some(root) = cache_root() else {
        return Ok(0);
    };
    prune_timed_json_cache(
        &root,
        unix_now_secs(),
        cache_ttl_secs(),
        extract_recorded_at,
    )
}

/// Summarize the dev-environment cache directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn dev_env_cache_status() -> io::Result<DevEnvironmentCacheStatus> {
    let enabled = dev_env_cache_enabled();
    let ttl_secs = cache_ttl_secs();
    let Some(root) = cache_root() else {
        return Ok(DevEnvironmentCacheStatus {
            path: String::new(),
            enabled,
            ttl_secs,
            entries: 0,
            total_bytes: 0,
            oldest_age_secs: None,
            newest_age_secs: None,
        });
    };

    if !root.is_dir() {
        return Ok(DevEnvironmentCacheStatus {
            path: root.display().to_string(),
            enabled,
            ttl_secs,
            entries: 0,
            total_bytes: 0,
            oldest_age_secs: None,
            newest_age_secs: None,
        });
    }

    let summary = summarize_timed_json_cache(&root, unix_now_secs(), extract_recorded_at)?;
    Ok(DevEnvironmentCacheStatus {
        path: root.display().to_string(),
        enabled,
        ttl_secs,
        entries: summary.entries,
        total_bytes: summary.total_bytes,
        oldest_age_secs: summary.oldest_age_secs,
        newest_age_secs: summary.newest_age_secs,
    })
}

/// Whether the snapshot still carries secret values that must not be persisted.
///
/// Checks every declared secret name: a real value in either [`DevEnvSecretVariable::value`]
/// or [`DevEnvironmentSnapshot::variables`] rejects persistence.
#[must_use]
pub fn snapshot_contains_secret_values(snapshot: &DevEnvironmentSnapshot) -> bool {
    snapshot.secret_variables.iter().any(|secret| {
        secret_variable_has_value(secret)
            || snapshot
                .variables
                .get(&secret.name)
                .is_some_and(|value| secret_value_is_real(value))
    })
}

/// Strip known secret names from [`DevEnvironmentSnapshot::variables`] and record
/// runtime placeholders in [`DevEnvironmentSnapshot::secret_variables`].
pub fn sanitize_snapshot_for_cache(
    snapshot: &mut DevEnvironmentSnapshot,
    known_secret_names: &[String],
) {
    for name in known_secret_names {
        snapshot.variables.remove(name);
        if let Some(slot) = snapshot
            .secret_variables
            .iter_mut()
            .find(|secret| secret.name == *name)
        {
            slot.value = DEV_ENV_SECRET_RUNTIME_PLACEHOLDER.to_owned();
        } else {
            snapshot.secret_variables.push(DevEnvSecretVariable {
                name: name.clone(),
                value: DEV_ENV_SECRET_RUNTIME_PLACEHOLDER.to_owned(),
            });
        }
    }
}

fn secret_variable_has_value(secret: &DevEnvSecretVariable) -> bool {
    secret_value_is_real(&secret.value)
}

fn secret_value_is_real(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed != DEV_ENV_SECRET_RUNTIME_PLACEHOLDER
}

fn unique_cache_temp_path(path: &std::path::Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| path.as_ref());
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("entry");
    let rand = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    parent.join(format!("{stem}.tmp.{}.{}", std::process::id(), rand))
}

#[cfg(unix)]
fn restrict_cache_dir_permissions(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_cache_dir_permissions(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_cache_file_permissions(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_cache_file_permissions(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

fn extract_recorded_at(contents: &str) -> Option<u64> {
    serde_json::from_str::<CachedDevEnvironmentSnapshot>(contents)
        .ok()
        .map(|cached| cached.recorded_at)
}

fn load_cached_snapshot(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> io::Result<Option<DevEnvironmentCacheHit>> {
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(None);
    };
    record_fs_metadata();
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let cached: CachedDevEnvironmentSnapshot = match serde_json::from_str(&contents) {
        Ok(cached) => cached,
        Err(_) => return Ok(None),
    };
    if cached.schema_version != DEV_ENV_CACHE_SCHEMA_VERSION {
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
    if snapshot_contains_secret_values(&cached.snapshot) {
        return Ok(None);
    }
    Ok(Some(DevEnvironmentCacheHit {
        snapshot: cached.snapshot,
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

    directories::ProjectDirs::from("dev", "nxr", "nxr").map(|dirs| dirs.cache_dir().join("dev-env"))
}

fn cache_ttl_secs() -> Option<u64> {
    #[cfg(test)]
    if let Some(override_ttl) = TEST_CACHE_TTL_SECS.with(|cell| *cell.borrow()) {
        return override_ttl;
    }

    match std::env::var(DEV_ENV_CACHE_TTL_ENV) {
        Ok(raw) => {
            let Ok(secs) = raw.parse::<u64>() else {
                return Some(DEFAULT_DEV_ENV_CACHE_TTL_SECS);
            };
            if secs == 0 { None } else { Some(secs) }
        }
        Err(_) => Some(DEFAULT_DEV_ENV_CACHE_TTL_SECS),
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
fn test_with_cache_enabled<T>(f: impl FnOnce() -> T) -> T {
    TEST_CACHE_ENABLED.with(|cell| {
        *cell.borrow_mut() = Some(true);
    });
    let result = f();
    TEST_CACHE_ENABLED.with(|cell| {
        *cell.borrow_mut() = None;
    });
    result
}

#[cfg(test)]
fn test_with_dev_env_cache<T>(root: PathBuf, f: impl FnOnce() -> T) -> T {
    test_with_cache_enabled(|| test_with_cache_dir(root, f))
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
            source_identity: Some("src".to_owned()),
        }
    }

    fn sample_nix_identity() -> DevEnvironmentNixIdentity {
        DevEnvironmentNixIdentity {
            nix_path: "/nix/bin/nix".to_owned(),
            nix_version: "2.34.0".to_owned(),
            nix_file_identity: Some("1:2:3".to_owned()),
            nix_tree_fingerprint: "tree".to_owned(),
        }
    }

    fn sample_snapshot(fingerprints: PlanCacheSharedFingerprints) -> DevEnvironmentSnapshot {
        DevEnvironmentSnapshot {
            flake_identity: "github:org/repo".to_owned(),
            system: "aarch64-darwin".to_owned(),
            shell: "default".to_owned(),
            nix_identity: sample_nix_identity(),
            variables: BTreeMap::from([
                ("PATH".to_owned(), "/nix/store/bin".to_owned()),
                ("IN_NIX_SHELL".to_owned(), "1".to_owned()),
            ]),
            path_entries: vec!["/nix/store/bin".to_owned()],
            unsupported_features: Vec::new(),
            fingerprints,
            protocol_version: DEV_ENV_PROTOCOL_VERSION,
            secret_variables: Vec::new(),
        }
    }

    fn sample_key(
        fingerprints: PlanCacheSharedFingerprints,
        environment_mode: &str,
    ) -> DevEnvironmentCacheKeyMaterial {
        DevEnvironmentCacheKeyMaterial {
            flake_identity: "github:org/repo".to_owned(),
            local_root: "/proj".to_owned(),
            system: "aarch64-darwin".to_owned(),
            shell_name: "default".to_owned(),
            environment_mode: environment_mode.to_owned(),
            nix_flags_digest: "flags".to_owned(),
            fingerprints,
            protocol_version: DEV_ENV_PROTOCOL_VERSION,
        }
    }

    #[test]
    fn hit_miss_and_clear() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("dev-env");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_dev_env_cache(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
            assert!(lookup_dev_environment_snapshot(&key, &fingerprints).is_none());

            let snapshot = sample_snapshot(fingerprints.clone());
            store_dev_environment_snapshot(&key, &snapshot, fingerprints.clone()).expect("store");

            let hit = lookup_dev_environment_snapshot(&key, &fingerprints).expect("hit");
            assert_eq!(hit.snapshot.shell, "default");
            assert_eq!(
                hit.snapshot.variables.get("PATH").map(String::as_str),
                Some("/nix/store/bin")
            );

            let mut stale = fingerprints.clone();
            stale.nix_tree_fingerprint = "changed".to_owned();
            assert!(lookup_dev_environment_snapshot(&key, &stale).is_none());

            assert_eq!(clear_dev_env_cache().expect("clear"), 1);
            assert!(lookup_dev_environment_snapshot(&key, &fingerprints).is_none());
        });
    }

    #[test]
    fn rejects_secret_values_on_store_and_load() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("dev-env");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_dev_env_cache(cache_home.clone(), || {
            let fingerprints = sample_fingerprints();
            let key = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
            let mut snapshot = sample_snapshot(fingerprints.clone());
            snapshot.secret_variables.push(DevEnvSecretVariable {
                name: "API_TOKEN".to_owned(),
                value: "super-secret".to_owned(),
            });
            store_dev_environment_snapshot(&key, &snapshot, fingerprints.clone())
                .expect("store no-ops");
            assert!(lookup_dev_environment_snapshot(&key, &fingerprints).is_none());

            // Manually write a poisoned entry; load must reject it.
            let path = cache_home.join(format!("{key}.json"));
            let poisoned = CachedDevEnvironmentSnapshot {
                schema_version: DEV_ENV_CACHE_SCHEMA_VERSION,
                key_digest: key.clone(),
                snapshot,
                fingerprints: fingerprints.clone(),
                recorded_at: unix_now_secs(),
            };
            fs::write(&path, serde_json::to_vec_pretty(&poisoned).expect("json")).expect("write");
            assert!(lookup_dev_environment_snapshot(&key, &fingerprints).is_none());
        });
    }

    #[test]
    fn key_stable_and_excludes_snapshot_payload() {
        let fingerprints = sample_fingerprints();
        let material = sample_key(fingerprints.clone(), "process");
        let digest_a = dev_env_cache_key_digest(&material);
        let digest_b = dev_env_cache_key_digest(&material);
        assert_eq!(digest_a, digest_b);

        let mut other_mode = material.clone();
        other_mode.environment_mode = "shell".to_owned();
        assert_ne!(digest_a, dev_env_cache_key_digest(&other_mode));

        let mut other_shell = material.clone();
        other_shell.shell_name = "ci".to_owned();
        assert_ne!(digest_a, dev_env_cache_key_digest(&other_shell));

        let mut other_protocol = material.clone();
        other_protocol.protocol_version = DEV_ENV_PROTOCOL_VERSION + 1;
        assert_ne!(digest_a, dev_env_cache_key_digest(&other_protocol));

        // Snapshot variables/path entries do not participate in the key.
        let snapshot_a = sample_snapshot(fingerprints.clone());
        let mut snapshot_b = sample_snapshot(fingerprints);
        snapshot_b
            .variables
            .insert("EXTRA".to_owned(), "value".to_owned());
        snapshot_b.path_entries.push("/other".to_owned());
        let key_a =
            dev_env_cache_key_digest(&sample_key(snapshot_a.fingerprints.clone(), "process"));
        let key_b = dev_env_cache_key_digest(&sample_key(snapshot_b.fingerprints, "process"));
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn key_changes_with_fingerprint_fields() {
        let fingerprints = sample_fingerprints();
        let a = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
        let mut stale = fingerprints;
        stale.flake_lock_digest = Some("other-lock".to_owned());
        let b = dev_env_cache_key_digest(&sample_key(stale, "process"));
        assert_ne!(a, b);
    }

    #[test]
    fn status_reports_entries() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("dev-env");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_dev_env_cache(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
            store_dev_environment_snapshot(
                &key,
                &sample_snapshot(fingerprints.clone()),
                fingerprints,
            )
            .expect("store");
            let status = dev_env_cache_status().expect("status");
            assert_eq!(status.entries, 1);
            assert!(status.total_bytes > 0);
            assert!(!status.path.is_empty());
        });
    }

    #[test]
    fn kill_switch_parses_opt_in_values() {
        assert!(!cache_enabled_for_env(None));
        assert!(cache_enabled_for_env(Some("on")));
        assert!(cache_enabled_for_env(Some("1")));
        assert!(cache_enabled_for_env(Some("true")));
        assert!(cache_enabled_for_env(Some("YES")));
        assert!(!cache_enabled_for_env(Some("off")));
        assert!(!cache_enabled_for_env(Some("0")));
        assert!(!cache_enabled_for_env(Some("false")));
        assert!(!cache_enabled_for_env(Some("NO")));
    }

    #[test]
    fn rejects_secret_value_in_variables_with_runtime_slot() {
        let fingerprints = sample_fingerprints();
        let mut snapshot = sample_snapshot(fingerprints);
        snapshot.secret_variables.push(DevEnvSecretVariable {
            name: "API_TOKEN".to_owned(),
            value: DEV_ENV_SECRET_RUNTIME_PLACEHOLDER.to_owned(),
        });
        snapshot
            .variables
            .insert("API_TOKEN".to_owned(), "super-secret".to_owned());
        assert!(snapshot_contains_secret_values(&snapshot));
    }

    #[test]
    fn sanitize_snapshot_strips_known_secret_names() {
        let fingerprints = sample_fingerprints();
        let mut snapshot = sample_snapshot(fingerprints);
        snapshot
            .variables
            .insert("API_TOKEN".to_owned(), "super-secret".to_owned());
        sanitize_snapshot_for_cache(&mut snapshot, &["API_TOKEN".to_owned()]);
        assert!(!snapshot.variables.contains_key("API_TOKEN"));
        assert_eq!(snapshot.secret_variables.len(), 1);
        assert_eq!(snapshot.secret_variables[0].name, "API_TOKEN");
        assert_eq!(
            snapshot.secret_variables[0].value,
            DEV_ENV_SECRET_RUNTIME_PLACEHOLDER
        );
        assert!(!snapshot_contains_secret_values(&snapshot));
    }

    #[test]
    fn sanitized_snapshot_can_be_stored_and_loaded() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("dev-env");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_dev_env_cache(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
            let mut snapshot = sample_snapshot(fingerprints.clone());
            snapshot
                .variables
                .insert("API_TOKEN".to_owned(), "super-secret".to_owned());
            sanitize_snapshot_for_cache(&mut snapshot, &["API_TOKEN".to_owned()]);
            store_dev_environment_snapshot(&key, &snapshot, fingerprints.clone()).expect("store");
            let hit = lookup_dev_environment_snapshot(&key, &fingerprints).expect("hit");
            assert!(!hit.snapshot.variables.contains_key("API_TOKEN"));
        });
    }

    #[test]
    fn poisoned_cache_file_never_serializes_secret_literal() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("dev-env");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_dev_env_cache(cache_home.clone(), || {
            let fingerprints = sample_fingerprints();
            let key = dev_env_cache_key_digest(&sample_key(fingerprints.clone(), "process"));
            let snapshot = sample_snapshot(fingerprints.clone());
            store_dev_environment_snapshot(&key, &snapshot, fingerprints).expect("store");
            let path = cache_home.join(format!("{key}.json"));
            let contents = fs::read_to_string(&path).expect("read");
            assert!(!contents.contains("super-secret"));
        });
    }
}
