//! Optional on-disk cache for prepared app command plans.
//!
//! Stores argv / plan envelopes keyed by flake + Nix + policy fingerprints.
//! Secret *values* are never persisted; entries with non-placeholder secret
//! fields are rejected. Disabled with `NXR_PLAN_CACHE=off` (or `0` / `false` /
//! `no`). See ADR-0152.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::plan::{Plan, PlanSecretRef};
use crate::{add_bytes_hashed, record_fs_metadata};

/// Environment variable disabling the prepared-plan cache (`off`, `0`, `false`, `no`).
pub const PLAN_CACHE_ENV: &str = "NXR_PLAN_CACHE";

/// On-disk schema version for prepared-plan cache entries.
pub const PLAN_CACHE_SCHEMA_VERSION: u32 = 1;

/// Placeholder written into [`PlanSecretRef::value`] (never a real secret).
pub const PLAN_SECRET_RUNTIME_PLACEHOLDER: &str = "<runtime>";

/// Default TTL backstop (24 hours). `NXR_PLAN_CACHE_TTL_SECS=0` disables TTL.
pub const DEFAULT_PLAN_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Environment variable overriding prepared-plan cache TTL in seconds.
pub const PLAN_CACHE_TTL_ENV: &str = "NXR_PLAN_CACHE_TTL_SECS";

/// Which prepare path produced the cached plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPrepareKind {
    /// Locate-only / synthetic app path (`prepare_fast_app_plan`).
    Fast,
    /// Discovery-backed path (`prepare_app_plan`).
    Discovered,
}

/// Fingerprint material shared with store-exe reuse (perf-1b / ADR-0153).
///
/// Callers compute these with existing discovery / lock / Nix identity helpers.
/// Values are digests or paths — never secret contents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanCacheSharedFingerprints {
    pub nix_tree_fingerprint: String,
    pub discovery_inputs_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake_lock_digest: Option<String>,
    pub nix_path: String,
    pub nix_version: String,
    /// Compact executable identity (`size:mtime_secs:mtime_nanos[:dev:ino]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nix_file_identity: Option<String>,
}

/// Inputs that identify a cached prepared plan (hashed into the file key).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanCacheKeyMaterial {
    pub prepare_kind: PlanPrepareKind,
    pub flake_ref: String,
    pub local_root: String,
    pub system: String,
    pub app_name: String,
    pub attr_path: String,
    pub nix_flags_digest: String,
    pub shell_name: Option<String>,
    pub shell_mode: String,
    pub active_shell: Option<String>,
    pub root: bool,
    pub cwd: Option<String>,
    pub invocation_directory: String,
    pub execution_directory: String,
    pub environment_policy_digest: String,
    pub forwarded_arguments: Vec<String>,
    pub fingerprints: PlanCacheSharedFingerprints,
}

/// On-disk prepared-plan cache summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanCacheStatus {
    pub path: String,
    pub entries: usize,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedPreparedPlan {
    schema_version: u32,
    key_digest: String,
    prepare_kind: PlanPrepareKind,
    plan: Plan,
    nix: String,
    execution_directory: String,
    fingerprints: PlanCacheSharedFingerprints,
    recorded_at: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_CACHE_TTL_SECS: std::cell::RefCell<Option<Option<u64>>> = const { std::cell::RefCell::new(None) };
}

/// Whether the prepared-plan disk cache is enabled.
#[must_use]
pub fn plan_cache_enabled() -> bool {
    cache_enabled_for_env(std::env::var(PLAN_CACHE_ENV).ok().as_deref())
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

/// Prepared-plan cache directory when the host provides a writable cache location.
#[must_use]
pub fn plan_cache_dir() -> Option<PathBuf> {
    cache_root()
}

/// BLAKE3 hex digest of [`PlanCacheKeyMaterial`].
#[must_use]
pub fn plan_cache_key_digest(material: &PlanCacheKeyMaterial) -> String {
    let mut hasher = Hasher::new();
    hash_str(&mut hasher, "v1");
    hash_prepare_kind(&mut hasher, material.prepare_kind);
    hash_str(&mut hasher, &material.flake_ref);
    hash_str(&mut hasher, &material.local_root);
    hash_str(&mut hasher, &material.system);
    hash_str(&mut hasher, &material.app_name);
    hash_str(&mut hasher, &material.attr_path);
    hash_str(&mut hasher, &material.nix_flags_digest);
    hash_opt_str(&mut hasher, material.shell_name.as_deref());
    hash_str(&mut hasher, &material.shell_mode);
    hash_opt_str(&mut hasher, material.active_shell.as_deref());
    hasher.update(&[u8::from(material.root)]);
    hasher.update(&[0]);
    hash_opt_str(&mut hasher, material.cwd.as_deref());
    hash_str(&mut hasher, &material.invocation_directory);
    hash_str(&mut hasher, &material.execution_directory);
    hash_str(&mut hasher, &material.environment_policy_digest);
    for arg in &material.forwarded_arguments {
        hash_str(&mut hasher, arg);
    }
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

/// Stable digest of optional Nix flags for cache keys.
#[must_use]
pub fn digest_nix_flags(
    offline: bool,
    no_write_lock_file: bool,
    accept_flake_config: bool,
    json_log_format: bool,
    nix_options: &[(String, String)],
    extra_argv: &[String],
) -> String {
    let mut hasher = Hasher::new();
    hasher.update(&[u8::from(offline), u8::from(no_write_lock_file)]);
    hasher.update(&[u8::from(accept_flake_config), u8::from(json_log_format)]);
    hasher.update(&[0]);
    for (key, value) in nix_options {
        hash_str(&mut hasher, key);
        hash_str(&mut hasher, value);
    }
    hasher.update(&[0]);
    for arg in extra_argv {
        hash_str(&mut hasher, arg);
    }
    hasher.finalize().to_hex().to_string()
}

/// Digest an [`EnvironmentPolicy`](crate::EnvironmentPolicy) for cache keys.
///
/// Includes CLI `--set` values (not secrets). Context secret values never appear
/// in environment policies used for app prepare.
#[must_use]
pub fn digest_environment_policy(policy: &crate::EnvironmentPolicy) -> String {
    match serde_json::to_vec(policy) {
        Ok(bytes) => {
            add_bytes_hashed(bytes.len() as u64);
            let mut hasher = Hasher::new();
            hasher.update(&bytes);
            hasher.finalize().to_hex().to_string()
        }
        Err(_) => "invalid-env-policy".to_owned(),
    }
}

/// Look up a prepared plan by key digest.
///
/// Returns `None` on miss, corruption, schema mismatch, TTL expiry, or when the
/// cache is disabled / unavailable. Fingerprints in `expected` must match the
/// stored bundle (defense in depth; key digest already includes them).
#[must_use]
pub fn lookup_prepared_plan(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> Option<PreparedPlanCacheHit> {
    if !plan_cache_enabled() {
        return None;
    }
    load_cached_plan(key_digest, expected).ok().flatten()
}

/// Store a prepared plan under `key_digest`.
///
/// No-ops when the cache is disabled, unavailable, or the plan contains secret
/// values (non-placeholder [`PlanSecretRef::value`]).
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory or entry cannot be written.
pub fn store_prepared_plan(
    key_digest: &str,
    prepare_kind: PlanPrepareKind,
    plan: &Plan,
    nix: &str,
    execution_directory: &str,
    fingerprints: PlanCacheSharedFingerprints,
) -> io::Result<()> {
    if !plan_cache_enabled() {
        return Ok(());
    }
    if plan_contains_secret_values(plan) {
        return Ok(());
    }
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let entry = CachedPreparedPlan {
        schema_version: PLAN_CACHE_SCHEMA_VERSION,
        key_digest: key_digest.to_owned(),
        prepare_kind,
        plan: plan.clone(),
        nix: nix.to_owned(),
        execution_directory: execution_directory.to_owned(),
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

/// Hit payload returned from [`lookup_prepared_plan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedPlanCacheHit {
    pub prepare_kind: PlanPrepareKind,
    pub plan: Plan,
    pub nix: String,
    pub execution_directory: String,
    pub fingerprints: PlanCacheSharedFingerprints,
}

/// Remove all prepared-plan cache entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn clear_plan_cache() -> io::Result<usize> {
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

/// Summarize the prepared-plan cache directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn plan_cache_status() -> io::Result<PlanCacheStatus> {
    let Some(root) = cache_root() else {
        return Ok(PlanCacheStatus {
            path: String::new(),
            entries: 0,
            total_bytes: 0,
        });
    };

    if !root.is_dir() {
        return Ok(PlanCacheStatus {
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

    Ok(PlanCacheStatus {
        path: root.display().to_string(),
        entries,
        total_bytes,
    })
}

fn plan_contains_secret_values(plan: &Plan) -> bool {
    plan.secrets.iter().any(secret_ref_has_value)
}

fn secret_ref_has_value(secret: &PlanSecretRef) -> bool {
    let trimmed = secret.value.trim();
    !trimmed.is_empty() && trimmed != PLAN_SECRET_RUNTIME_PLACEHOLDER
}

fn load_cached_plan(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> io::Result<Option<PreparedPlanCacheHit>> {
    let Some(path) = cache_file_path(key_digest) else {
        return Ok(None);
    };
    record_fs_metadata();
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let cached: CachedPreparedPlan = match serde_json::from_str(&contents) {
        Ok(cached) => cached,
        Err(_) => return Ok(None),
    };
    if cached.schema_version != PLAN_CACHE_SCHEMA_VERSION {
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
    if plan_contains_secret_values(&cached.plan) {
        return Ok(None);
    }
    Ok(Some(PreparedPlanCacheHit {
        prepare_kind: cached.prepare_kind,
        plan: cached.plan,
        nix: cached.nix,
        execution_directory: cached.execution_directory,
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

    directories::ProjectDirs::from("dev", "nxr", "nxr").map(|dirs| dirs.cache_dir().join("plans"))
}

fn cache_ttl_secs() -> Option<u64> {
    #[cfg(test)]
    if let Some(override_ttl) = TEST_CACHE_TTL_SECS.with(|cell| *cell.borrow()) {
        return override_ttl;
    }

    match std::env::var(PLAN_CACHE_TTL_ENV) {
        Ok(raw) => {
            let Ok(secs) = raw.parse::<u64>() else {
                return Some(DEFAULT_PLAN_CACHE_TTL_SECS);
            };
            if secs == 0 { None } else { Some(secs) }
        }
        Err(_) => Some(DEFAULT_PLAN_CACHE_TTL_SECS),
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

fn hash_prepare_kind(hasher: &mut Hasher, kind: PlanPrepareKind) {
    let label = match kind {
        PlanPrepareKind::Fast => "fast",
        PlanPrepareKind::Discovered => "discovered",
    };
    hash_str(hasher, label);
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
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::EnvironmentPolicy;
    use crate::plan::{PlanCommand, PlanKind};

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

    fn sample_plan() -> Plan {
        Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: "/proj".to_owned(),
            system: "aarch64-darwin".to_owned(),
            target: "hello".to_owned(),
            attr_path: "apps.aarch64-darwin.hello".to_owned(),
            invocation_directory: "/proj".to_owned(),
            execution_directory: "/proj".to_owned(),
            shell: None,
            active_shell: None,
            environment_policy: EnvironmentPolicy::Inherit,
            context: None,
            secrets: Vec::new(),
            context_env_set: BTreeMap::new(),
            command: PlanCommand {
                program: "/nix/bin/nix".to_owned(),
                arguments: vec!["run".to_owned(), "/proj#hello".to_owned()],
            },
            forwarded_arguments: Vec::new(),
        }
    }

    fn sample_key(
        fingerprints: PlanCacheSharedFingerprints,
        forwarded: Vec<String>,
    ) -> PlanCacheKeyMaterial {
        PlanCacheKeyMaterial {
            prepare_kind: PlanPrepareKind::Discovered,
            flake_ref: "/proj".to_owned(),
            local_root: "/proj".to_owned(),
            system: "aarch64-darwin".to_owned(),
            app_name: "hello".to_owned(),
            attr_path: "apps.aarch64-darwin.hello".to_owned(),
            nix_flags_digest: "flags".to_owned(),
            shell_name: None,
            shell_mode: "smart".to_owned(),
            active_shell: None,
            root: false,
            cwd: None,
            invocation_directory: "/proj".to_owned(),
            execution_directory: "/proj".to_owned(),
            environment_policy_digest: "env".to_owned(),
            forwarded_arguments: forwarded,
            fingerprints,
        }
    }

    #[test]
    fn hit_miss_and_clear() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("plans");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_cache_dir(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = plan_cache_key_digest(&sample_key(fingerprints.clone(), Vec::new()));
            assert!(lookup_prepared_plan(&key, &fingerprints).is_none());

            let plan = sample_plan();
            store_prepared_plan(
                &key,
                PlanPrepareKind::Discovered,
                &plan,
                "/nix/bin/nix",
                "/proj",
                fingerprints.clone(),
            )
            .expect("store");

            let hit = lookup_prepared_plan(&key, &fingerprints).expect("hit");
            assert_eq!(hit.plan.target, "hello");
            assert_eq!(hit.nix, "/nix/bin/nix");

            let mut stale = fingerprints.clone();
            stale.nix_tree_fingerprint = "changed".to_owned();
            assert!(lookup_prepared_plan(&key, &stale).is_none());

            assert_eq!(clear_plan_cache().expect("clear"), 1);
            assert!(lookup_prepared_plan(&key, &fingerprints).is_none());
        });
    }

    #[test]
    fn rejects_secret_values_on_store_and_load() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("plans");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_cache_dir(cache_home.clone(), || {
            let fingerprints = sample_fingerprints();
            let key = plan_cache_key_digest(&sample_key(fingerprints.clone(), Vec::new()));
            let mut plan = sample_plan();
            plan.secrets.push(PlanSecretRef {
                name: "token".to_owned(),
                reference: "TOKEN".to_owned(),
                delivery: "env".to_owned(),
                provider: "env".to_owned(),
                value: "super-secret".to_owned(),
            });
            store_prepared_plan(
                &key,
                PlanPrepareKind::Discovered,
                &plan,
                "/nix/bin/nix",
                "/proj",
                fingerprints.clone(),
            )
            .expect("store no-ops");
            assert!(lookup_prepared_plan(&key, &fingerprints).is_none());

            // Manually write a poisoned entry; load must reject it.
            let path = cache_home.join(format!("{key}.json"));
            let poisoned = CachedPreparedPlan {
                schema_version: PLAN_CACHE_SCHEMA_VERSION,
                key_digest: key.clone(),
                prepare_kind: PlanPrepareKind::Discovered,
                plan,
                nix: "/nix/bin/nix".to_owned(),
                execution_directory: "/proj".to_owned(),
                fingerprints: fingerprints.clone(),
                recorded_at: unix_now_secs(),
            };
            fs::write(&path, serde_json::to_vec_pretty(&poisoned).expect("json")).expect("write");
            assert!(lookup_prepared_plan(&key, &fingerprints).is_none());
        });
    }

    #[test]
    fn key_changes_with_forwarded_args_and_flags() {
        let fingerprints = sample_fingerprints();
        let a = plan_cache_key_digest(&sample_key(fingerprints.clone(), Vec::new()));
        let b = plan_cache_key_digest(&sample_key(fingerprints, vec!["--check".to_owned()]));
        assert_ne!(a, b);

        let flags_a = digest_nix_flags(false, false, false, false, &[], &[]);
        let flags_b = digest_nix_flags(true, false, false, false, &[], &[]);
        assert_ne!(flags_a, flags_b);
    }

    #[test]
    fn status_reports_entries() {
        let temp = TempDir::new().expect("tempdir");
        let cache_home = temp.path().join("plans");
        fs::create_dir_all(&cache_home).expect("mkdir");
        test_with_cache_dir(cache_home, || {
            let fingerprints = sample_fingerprints();
            let key = plan_cache_key_digest(&sample_key(fingerprints.clone(), Vec::new()));
            store_prepared_plan(
                &key,
                PlanPrepareKind::Fast,
                &sample_plan(),
                "/nix/bin/nix",
                "/proj",
                fingerprints,
            )
            .expect("store");
            let status = plan_cache_status().expect("status");
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
}
