//! Discovery metadata cache keyed by local flake inputs.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use nxr_core::App;
use nxr_task::{SCHEMA_VERSION as DISCOVERY_SCHEMA_VERSION, TaskDocument};
use serde::{Deserialize, Serialize};

use crate::fingerprint::{discovery_inputs_fingerprint, nix_tree_fingerprint};

/// Environment variable overriding the discovery cache TTL in seconds.
///
/// Unset → default [`DEFAULT_CACHE_TTL_SECS`] (24h). `0` disables the TTL backstop.
pub const CACHE_TTL_ENV: &str = "NXR_CACHE_TTL_SECS";

/// Default discovery cache TTL (24 hours).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Inputs that identify a cached discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryContext {
    pub flake_ref: String,
    pub local_root: Option<Utf8PathBuf>,
    pub system: String,
    /// Canonical Nix executable path. Empty uses a test stub or resolves at cache time.
    pub nix_path: String,
    /// Nix version string (for example `2.34.7`). Empty pairs with [`Self::nix_path`].
    pub nix_version: String,
    /// Extra flake-root-relative paths to content-hash (`perSystem.nxr.discoveryInputs`).
    pub discovery_inputs: Vec<String>,
}

impl DiscoveryContext {
    /// Build a context with empty Nix identity and discovery inputs.
    #[must_use]
    pub fn new(
        flake_ref: impl Into<String>,
        local_root: Option<Utf8PathBuf>,
        system: impl Into<String>,
    ) -> Self {
        Self {
            flake_ref: flake_ref.into(),
            local_root,
            system: system.into(),
            nix_path: String::new(),
            nix_version: String::new(),
            discovery_inputs: Vec::new(),
        }
    }

    /// Attach Nix executable identity used in the cache key.
    #[must_use]
    pub fn with_nix_identity(
        mut self,
        nix_path: impl Into<String>,
        nix_version: impl Into<String>,
    ) -> Self {
        self.nix_path = nix_path.into();
        self.nix_version = nix_version.into();
        self
    }

    /// Attach extra content-hashed discovery input paths.
    #[must_use]
    pub fn with_discovery_inputs(mut self, inputs: Vec<String>) -> Self {
        self.discovery_inputs = inputs;
        self
    }
}

/// Options controlling cache lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryCacheOptions {
    pub refresh: bool,
    /// When true, a cache entry without tasks is treated as a miss.
    pub require_tasks: bool,
}

impl DiscoveryCacheOptions {
    #[must_use]
    pub const fn fresh() -> Self {
        Self {
            refresh: true,
            require_tasks: false,
        }
    }

    #[must_use]
    pub const fn normal() -> Self {
        Self {
            refresh: false,
            require_tasks: false,
        }
    }

    #[must_use]
    pub const fn with_tasks(refresh: bool) -> Self {
        Self {
            refresh,
            require_tasks: true,
        }
    }
}

/// Apps plus optional tasks discovered for one flake/system.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDiscovery {
    pub apps: Vec<App>,
    pub tasks: Option<TaskDocument>,
}

/// Return cached apps when the on-disk entry is still valid.
///
/// Returns `None` on cache miss, corruption, or when the flake is remote.
#[must_use]
pub fn cached_apps(context: &DiscoveryContext) -> Option<Vec<App>> {
    cached_workspace(context).map(|discovery| discovery.apps)
}

/// Return cached apps and tasks when the on-disk entry is still valid.
#[must_use]
pub fn cached_workspace(context: &DiscoveryContext) -> Option<WorkspaceDiscovery> {
    let local_root = context.local_root.as_ref()?;
    load_cached_workspace(local_root, context, false)
        .ok()
        .flatten()
}

/// Return cached workspace data when valid, otherwise run `discover` and update the cache.
///
/// Remote flakes (no `local_root`) always call `discover` directly. Cache read and
/// write failures are treated as cache misses or no-ops so discovery still succeeds.
///
/// # Errors
///
/// Returns the error from `discover` when a fresh evaluation is required.
pub fn discover_workspace_with_cache<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Result<WorkspaceDiscovery, E>
where
    F: FnOnce() -> Result<WorkspaceDiscovery, E>,
{
    if options.refresh {
        let discovery = discover()?;
        if let Some(local_root) = &context.local_root {
            let _ = store_cached_workspace(local_root, context, &discovery);
        }
        return Ok(discovery);
    }

    let Some(local_root) = &context.local_root else {
        return discover();
    };

    if let Ok(Some(discovery)) = load_cached_workspace(local_root, context, options.require_tasks) {
        return Ok(discovery);
    }

    let discovery = discover()?;
    let _ = store_cached_workspace(local_root, context, &discovery);
    Ok(discovery)
}

/// Return cached apps when valid, otherwise run `discover` and update the cache.
///
/// # Errors
///
/// Returns the error from `discover` when a fresh evaluation is required.
pub fn discover_with_cache<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Result<Vec<App>, E>
where
    F: FnOnce() -> Result<Vec<App>, E>,
{
    let discovery = discover_workspace_with_cache(context, options, || {
        discover().map(|apps| WorkspaceDiscovery { apps, tasks: None })
    })?;
    Ok(discovery.apps)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedDiscovery {
    schema_version: u32,
    flake_root: String,
    nix_fingerprint: u64,
    nix_path: String,
    nix_version: String,
    discovery_schema_version: u32,
    /// Sorted flake-root-relative paths from `discoveryInputs` at store time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    discovery_inputs: Vec<String>,
    discovery_inputs_fingerprint: u64,
    /// Unix seconds when the entry was written (TTL backstop).
    cached_at: u64,
    system: String,
    flake_ref: String,
    apps: Vec<App>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tasks: Option<TaskDocument>,
}

const CACHE_SCHEMA_VERSION: u32 = 3;

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    // Outer Option: unset vs override; inner Option: disabled TTL vs Some(secs).
    #[allow(clippy::option_option)]
    static TEST_CACHE_TTL_SECS: std::cell::RefCell<Option<Option<u64>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
static CONCURRENT_TEST_CACHE_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

fn cache_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_CACHE_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    #[cfg(test)]
    if let Ok(guard) = CONCURRENT_TEST_CACHE_ROOT.lock()
        && let Some(root) = guard.clone()
    {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("discovery"))
}

/// Discovery cache directory when the host provides a writable cache location.
#[must_use]
pub fn discovery_cache_dir() -> Option<PathBuf> {
    cache_root()
}

/// On-disk discovery cache summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct DiscoveryCacheStatus {
    pub path: String,
    pub entries: usize,
    pub total_bytes: u64,
}

/// Per-flake discovery cache metadata for explain / doctor diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryCacheEntry {
    /// Whether a local flake root enables discovery caching.
    pub available: bool,
    /// Discovery cache directory (empty when unavailable).
    pub directory: String,
    /// Cache file path for this flake/system context, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_file: Option<String>,
    /// Whether a valid cache entry exists for the current inputs.
    pub hit: bool,
    /// Current content fingerprint used for invalidation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidation_key: Option<u64>,
    /// Fingerprint stored in the on-disk entry (present on hit or stale miss).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_invalidation_key: Option<u64>,
}

/// Remove all discovery cache entries.
///
/// Returns the number of cache files removed. Missing cache directories are
/// treated as empty.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries
/// cannot be removed.
pub fn clear_discovery_cache() -> io::Result<usize> {
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

/// Summarize the discovery cache directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn discovery_cache_status() -> io::Result<DiscoveryCacheStatus> {
    let Some(root) = cache_root() else {
        return Ok(DiscoveryCacheStatus {
            path: String::new(),
            entries: 0,
            total_bytes: 0,
        });
    };

    if !root.is_dir() {
        return Ok(DiscoveryCacheStatus {
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

    Ok(DiscoveryCacheStatus {
        path: root.display().to_string(),
        entries,
        total_bytes,
    })
}

/// Inspect discovery cache validity and invalidation keys for one flake context.
///
/// Remote flakes (`local_root` absent) return `available: false` with no keys.
///
/// # Errors
///
/// Returns [`io::Error`] when fingerprinting or reading a stale cache file fails.
pub fn discovery_cache_entry(context: &DiscoveryContext) -> io::Result<DiscoveryCacheEntry> {
    let directory = discovery_cache_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    let Some(local_root) = &context.local_root else {
        return Ok(DiscoveryCacheEntry {
            available: false,
            directory,
            cache_file: None,
            hit: false,
            invalidation_key: None,
            cached_invalidation_key: None,
        });
    };

    let canonical_root = canonical_flake_root(local_root);
    let invalidation_key = nix_tree_fingerprint(&canonical_root)?;
    let context_key = cache_context_key(context);
    let cache_file = cache_file_path(&context_key).map(|path| path.display().to_string());
    let hit = cached_workspace(context).is_some();
    let cached_invalidation_key = if hit {
        Some(invalidation_key)
    } else {
        cache_file
            .as_ref()
            .and_then(|path| read_cached_fingerprint(path))
            .transpose()?
    };

    Ok(DiscoveryCacheEntry {
        available: true,
        directory,
        cache_file,
        hit,
        invalidation_key: Some(invalidation_key),
        cached_invalidation_key,
    })
}

fn read_cached_fingerprint(cache_file: &str) -> Option<io::Result<u64>> {
    let contents = match fs::read_to_string(cache_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => return Some(Err(error)),
    };
    let cached: CachedDiscovery = match serde_json::from_str(&contents) {
        Ok(cached) => cached,
        Err(error) => {
            return Some(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
        }
    };
    Some(Ok(cached.nix_fingerprint))
}

fn cache_file_path(context: &DiscoveryContext) -> Option<PathBuf> {
    let root = cache_root()?;
    Some(root.join(cache_file_name(&cache_context_key(context))))
}

fn cache_context_key(context: &DiscoveryContext) -> DiscoveryContext {
    let local_root = context
        .local_root
        .as_ref()
        .map(|path| canonical_flake_root(path));
    let flake_ref = local_root.as_ref().map_or_else(
        || context.flake_ref.clone(),
        |root| root.as_str().to_owned(),
    );
    let (nix_path, nix_version) = effective_nix_identity(context);
    let mut discovery_inputs = context.discovery_inputs.clone();
    discovery_inputs.sort();
    discovery_inputs.dedup();
    DiscoveryContext {
        flake_ref,
        local_root,
        system: context.system.clone(),
        nix_path,
        nix_version,
        discovery_inputs,
    }
}

fn cache_file_name(context: &DiscoveryContext) -> String {
    let mut hasher = DefaultHasher::new();
    context.local_root.hash(&mut hasher);
    context.system.hash(&mut hasher);
    context.flake_ref.hash(&mut hasher);
    context.nix_path.hash(&mut hasher);
    context.nix_version.hash(&mut hasher);
    DISCOVERY_SCHEMA_VERSION.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

fn canonical_flake_root(local_root: &Utf8Path) -> Utf8PathBuf {
    local_root
        .canonicalize_utf8()
        .unwrap_or_else(|_| local_root.to_path_buf())
}

fn effective_nix_identity(context: &DiscoveryContext) -> (String, String) {
    if !context.nix_path.is_empty() {
        let path = Path::new(&context.nix_path).canonicalize().map_or_else(
            |_| context.nix_path.clone(),
            |path| path.display().to_string(),
        );
        let version = if context.nix_version.is_empty() {
            default_nix_version()
        } else {
            context.nix_version.clone()
        };
        return (path, version);
    }

    #[cfg(test)]
    {
        ("test-nix".to_owned(), "0.0.0".to_owned())
    }

    #[cfg(not(test))]
    {
        resolve_nix_identity_from_env().unwrap_or_else(|_| ("nix".to_owned(), "unknown".to_owned()))
    }
}

fn default_nix_version() -> String {
    #[cfg(test)]
    {
        "0.0.0".to_owned()
    }
    #[cfg(not(test))]
    {
        "unknown".to_owned()
    }
}

#[cfg(not(test))]
fn resolve_nix_identity_from_env() -> io::Result<(String, String)> {
    let nix = std::env::var("NXR_NIX").unwrap_or_else(|_| "nix".to_owned());
    let path = PathBuf::from(&nix);
    let canonical = path.canonicalize().unwrap_or(path).display().to_string();
    let output = std::process::Command::new(&canonical)
        .arg("--version")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("nix --version failed"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next_back())
        .unwrap_or("unknown")
        .to_owned();
    Ok((canonical, version))
}

fn cache_ttl_secs() -> Option<u64> {
    #[cfg(test)]
    if let Some(override_ttl) = TEST_CACHE_TTL_SECS.with(|cell| *cell.borrow()) {
        return override_ttl;
    }

    match std::env::var(CACHE_TTL_ENV) {
        Ok(raw) => {
            let Ok(secs) = raw.parse::<u64>() else {
                return Some(DEFAULT_CACHE_TTL_SECS);
            };
            if secs == 0 { None } else { Some(secs) }
        }
        Err(_) => Some(DEFAULT_CACHE_TTL_SECS),
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn merged_discovery_inputs(
    context: &DiscoveryContext,
    discovery: &WorkspaceDiscovery,
) -> Vec<String> {
    let mut inputs = context.discovery_inputs.clone();
    if let Some(tasks) = &discovery.tasks {
        inputs.extend(tasks.discovery_inputs.iter().cloned());
    }
    inputs.retain(|path| !path.is_empty());
    inputs.sort();
    inputs.dedup();
    inputs
}

fn load_cached_workspace(
    local_root: &Utf8Path,
    context: &DiscoveryContext,
    require_tasks: bool,
) -> io::Result<Option<WorkspaceDiscovery>> {
    let context = cache_context_key(context);
    let path = cache_file_path(&context)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))?;

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let cached: CachedDiscovery = serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    if cached.schema_version != CACHE_SCHEMA_VERSION {
        return Ok(None);
    }

    if cached.discovery_schema_version != DISCOVERY_SCHEMA_VERSION {
        return Ok(None);
    }

    let canonical_root = canonical_flake_root(local_root);
    if cached.flake_root != canonical_root.as_str()
        || cached.system != context.system
        || cached.flake_ref != context.flake_ref
        || cached.nix_path != context.nix_path
        || cached.nix_version != context.nix_version
    {
        return Ok(None);
    }

    if let Some(ttl) = cache_ttl_secs()
        && unix_now_secs().saturating_sub(cached.cached_at) > ttl
    {
        return Ok(None);
    }

    let current_fingerprint = nix_tree_fingerprint(&canonical_root)?;
    if cached.nix_fingerprint != current_fingerprint {
        return Ok(None);
    }

    let inputs_fingerprint =
        discovery_inputs_fingerprint(&canonical_root, &cached.discovery_inputs)?;
    if cached.discovery_inputs_fingerprint != inputs_fingerprint {
        return Ok(None);
    }

    if require_tasks && cached.tasks.is_none() {
        return Ok(None);
    }

    Ok(Some(WorkspaceDiscovery {
        apps: cached.apps,
        tasks: cached.tasks,
    }))
}

fn store_cached_workspace(
    local_root: &Utf8Path,
    context: &DiscoveryContext,
    discovery: &WorkspaceDiscovery,
) -> io::Result<()> {
    let mut context = cache_context_key(context);
    let discovery_inputs = merged_discovery_inputs(&context, discovery);
    context.discovery_inputs.clone_from(&discovery_inputs);

    let path = cache_file_path(&context)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let canonical_root = canonical_flake_root(local_root);
    let nix_fingerprint = nix_tree_fingerprint(&canonical_root)?;
    let discovery_inputs_fingerprint =
        discovery_inputs_fingerprint(&canonical_root, &discovery_inputs)?;
    let entry = CachedDiscovery {
        schema_version: CACHE_SCHEMA_VERSION,
        flake_root: canonical_root.as_str().to_owned(),
        nix_fingerprint,
        nix_path: context.nix_path.clone(),
        nix_version: context.nix_version.clone(),
        discovery_schema_version: DISCOVERY_SCHEMA_VERSION,
        discovery_inputs,
        discovery_inputs_fingerprint,
        cached_at: unix_now_secs(),
        system: context.system.clone(),
        flake_ref: context.flake_ref.clone(),
        apps: discovery.apps.clone(),
        tasks: discovery.tasks.clone(),
    };

    let serialized = serde_json::to_vec_pretty(&entry)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    write_atomically(&path, &serialized)
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;

    // Serialize writers to the same cache entry. Concurrent renames onto the same
    // destination can otherwise race to ENOENT on some platforms.
    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache")
    ));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let temp_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        Ok(())
    })();

    let _ = fs::remove_file(&temp_path);
    lock_file.unlock()?;
    write_result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::thread;

    use nxr_core::App;
    use nxr_task::{TaskDefinition, TaskDocument};
    use serde_json::Value as JsonValue;
    use tempfile::TempDir;

    use super::{
        DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, cache_file_path,
        discover_with_cache, discover_workspace_with_cache, load_cached_workspace,
        store_cached_workspace,
    };

    fn test_context(root: &camino::Utf8Path, system: &str) -> DiscoveryContext {
        DiscoveryContext::new(root.as_str(), Some(root.to_path_buf()), system)
    }

    fn sample_apps(flake_ref: &str, system: &str) -> Vec<App> {
        vec![App {
            name: "hello".to_owned(),
            attr_path: format!("apps.{system}.hello"),
            flake_ref: flake_ref.to_owned(),
            system: system.to_owned(),
            description: Some("greet".to_owned()),
            is_default: false,
            metadata: BTreeMap::new(),
        }]
    }

    fn sample_tasks() -> TaskDocument {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: Some("CI".to_owned()),
                depends_on: Vec::new(),
                app: "hello".to_owned(),
                working_directory: None,
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
            },
        );
        TaskDocument::new(tasks)
    }

    fn with_cache_dir<T>(temp: &TempDir, f: impl FnOnce() -> T) -> T {
        let cache_home = temp.path().join("cache").join("discovery");
        fs::create_dir_all(&cache_home).expect("create cache dir");
        super::TEST_CACHE_ROOT.with(|cell| {
            *cell.borrow_mut() = Some(cache_home);
        });

        let result = f();

        super::TEST_CACHE_ROOT.with(|cell| {
            *cell.borrow_mut() = None;
        });
        result
    }

    fn with_shared_cache_dir<T>(cache_home: &Path, f: impl FnOnce() -> T) -> T {
        fs::create_dir_all(cache_home).expect("create cache dir");
        {
            let mut guard = super::CONCURRENT_TEST_CACHE_ROOT
                .lock()
                .expect("concurrent cache lock");
            *guard = Some(cache_home.to_path_buf());
        }

        let result = f();

        {
            let mut guard = super::CONCURRENT_TEST_CACHE_ROOT
                .lock()
                .expect("concurrent cache lock");
            *guard = None;
        }
        result
    }

    fn write_flake(root: &camino::Utf8Path) {
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");
    }

    fn write_flake_with_import(root: &camino::Utf8Path) {
        fs::write(root.join("flake.nix"), "import ./nix/apps.nix").expect("write flake.nix");
        fs::create_dir_all(root.join("nix")).expect("nix dir");
        fs::write(root.join("nix/apps.nix"), "{ }").expect("apps.nix");
    }

    #[test]
    fn cache_miss_runs_discover_and_stores_result() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            let mut calls = 0;
            let discovered = discover_with_cache(&context, DiscoveryCacheOptions::normal(), || {
                calls += 1;
                Ok::<_, std::convert::Infallible>(apps.clone())
            })
            .expect("discover");

            assert_eq!(calls, 1);
            assert_eq!(discovered, apps);

            let cached = load_cached_workspace(&root, &context, false)
                .expect("read cache")
                .expect("cache hit");
            assert_eq!(cached.apps, apps);

            let mut calls = 0;
            let hit = discover_with_cache(&context, DiscoveryCacheOptions::normal(), || {
                calls += 1;
                Ok::<_, std::convert::Infallible>(Vec::new())
            })
            .expect("cache hit");

            assert_eq!(calls, 0);
            assert_eq!(hit, apps);
        });
    }

    #[test]
    fn combined_snapshot_round_trips_apps_and_tasks() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");
        let tasks = sample_tasks();

        with_cache_dir(&temp, || {
            let discovery = WorkspaceDiscovery {
                apps: apps.clone(),
                tasks: Some(tasks.clone()),
            };
            store_cached_workspace(&root, &context, &discovery).expect("store cache");

            let cached = load_cached_workspace(&root, &context, true)
                .expect("read cache")
                .expect("cache hit");
            assert_eq!(cached.apps, apps);
            assert_eq!(cached.tasks, Some(tasks));
        });
    }

    #[test]
    fn require_tasks_treats_apps_only_entry_as_miss() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("store cache");

            assert!(
                load_cached_workspace(&root, &context, true)
                    .expect("read cache")
                    .is_none()
            );
            assert!(
                load_cached_workspace(&root, &context, false)
                    .expect("read cache")
                    .is_some()
            );
        });
    }

    #[test]
    fn workspace_cache_hit_skips_discover() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");
        let tasks = sample_tasks();

        with_cache_dir(&temp, || {
            let mut calls = 0;
            let first = discover_workspace_with_cache(
                &context,
                DiscoveryCacheOptions::with_tasks(false),
                || {
                    calls += 1;
                    Ok::<_, std::convert::Infallible>(WorkspaceDiscovery {
                        apps: apps.clone(),
                        tasks: Some(tasks.clone()),
                    })
                },
            )
            .expect("first discover");
            assert_eq!(calls, 1);
            assert_eq!(first.tasks, Some(tasks.clone()));

            let mut calls = 0;
            let hit = discover_workspace_with_cache(
                &context,
                DiscoveryCacheOptions::with_tasks(false),
                || {
                    calls += 1;
                    Ok::<_, std::convert::Infallible>(WorkspaceDiscovery {
                        apps: Vec::new(),
                        tasks: None,
                    })
                },
            )
            .expect("cache hit");
            assert_eq!(calls, 0);
            assert_eq!(hit.apps, apps);
            assert_eq!(hit.tasks, Some(tasks));
        });
    }

    #[test]
    fn refresh_bypasses_cache_and_replaces_entry() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let initial = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: initial.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            let refreshed = vec![App {
                name: "deploy".to_owned(),
                attr_path: "apps.aarch64-darwin.deploy".to_owned(),
                flake_ref: root.as_str().to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            }];

            let mut calls = 0;
            let apps = discover_with_cache(&context, DiscoveryCacheOptions::fresh(), || {
                calls += 1;
                Ok::<_, std::convert::Infallible>(refreshed.clone())
            })
            .expect("refresh discover");

            assert_eq!(calls, 1);
            assert_eq!(apps, refreshed);

            let cached = load_cached_workspace(&root, &context, false)
                .expect("read cache")
                .expect("cache entry");
            assert_eq!(cached.apps, refreshed);
        });
    }

    #[test]
    fn imported_nix_change_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake_with_import(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            fs::write(root.join("nix/apps.nix"), "{ changed = true; }").expect("edit apps.nix");

            let cached = load_cached_workspace(&root, &context, false).expect("read cache");
            assert!(cached.is_none());

            let mut calls = 0;
            let rediscovered =
                discover_with_cache(&context, DiscoveryCacheOptions::normal(), || {
                    calls += 1;
                    Ok::<_, std::convert::Infallible>(apps.clone())
                })
                .expect("rediscover");
            assert_eq!(calls, 1);
            assert_eq!(rediscovered, apps);
        });
    }

    #[test]
    fn flake_lock_atomic_replace_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        fs::write(root.join("flake.lock"), "{}").expect("write flake.lock");
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            let temp_lock = root.join(".flake.lock.tmp");
            fs::write(&temp_lock, "{ \"nodes\": {} }").expect("new lock");
            fs::rename(&temp_lock, root.join("flake.lock")).expect("atomic replace");

            assert!(
                load_cached_workspace(&root, &context, false)
                    .expect("read cache")
                    .is_none()
            );
        });
    }

    #[test]
    fn symlink_flake_root_hits_canonical_cache_entry() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        let links = temp.path().join("links");
        fs::create_dir_all(&links).expect("links dir");
        let link = links.join("flake-link");
        std::os::unix::fs::symlink(&root, &link).expect("symlink");
        let link_root = camino::Utf8PathBuf::from_path_buf(link).expect("utf8 link path");
        let context = test_context(&link_root, "aarch64-darwin");
        let canonical_context = test_context(&root, "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &canonical_context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            let cached = load_cached_workspace(&link_root, &context, false)
                .expect("read cache")
                .expect("cache hit via symlink");
            assert_eq!(cached.apps, apps);
        });
    }

    #[test]
    fn remote_flake_skips_cache() {
        let temp = TempDir::new().expect("tempdir");
        let context = DiscoveryContext::new("github:owner/repo", None, "aarch64-darwin");

        with_cache_dir(&temp, || {
            let mut calls = 0;
            let apps = discover_with_cache(&context, DiscoveryCacheOptions::normal(), || {
                calls += 1;
                Ok::<_, std::convert::Infallible>(sample_apps(
                    "github:owner/repo",
                    "aarch64-darwin",
                ))
            })
            .expect("discover remote");

            assert_eq!(calls, 1);
            assert_eq!(apps.len(), 1);

            let cache_dir = cache_file_path(&context).expect("cache path");
            assert!(
                !cache_dir.exists(),
                "remote flakes should not create cache entries"
            );
        });
    }

    #[test]
    fn corrupt_cache_entry_is_treated_as_miss() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            let path = cache_file_path(&context).expect("cache path");
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("cache dir");
            }
            fs::write(path, "not-json").expect("write corrupt cache");

            let mut calls = 0;
            let discovered = discover_with_cache(&context, DiscoveryCacheOptions::normal(), || {
                calls += 1;
                Ok::<_, std::convert::Infallible>(apps.clone())
            })
            .expect("discover after corrupt cache");

            assert_eq!(calls, 1);
            assert_eq!(discovered, apps);
        });
    }

    #[test]
    fn concurrent_writers_leave_valid_cache_entry() {
        let temp = TempDir::new().expect("tempdir");
        let cache_temp = TempDir::new().expect("cache tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        // Keep the cache outside the flake root so fingerprint walks do not race
        // with concurrent cache file creation.
        let cache_home = cache_temp.path().join("discovery");

        with_shared_cache_dir(&cache_home, || {
            let handles: Vec<_> = (0..8)
                .map(|index| {
                    let root = root.clone();
                    let context = context.clone();
                    thread::spawn(move || {
                        let apps = vec![App {
                            name: format!("app-{index}"),
                            attr_path: format!("apps.aarch64-darwin.app-{index}"),
                            flake_ref: root.as_str().to_owned(),
                            system: "aarch64-darwin".to_owned(),
                            description: None,
                            is_default: false,
                            metadata: BTreeMap::new(),
                        }];
                        store_cached_workspace(
                            &root,
                            &context,
                            &WorkspaceDiscovery { apps, tasks: None },
                        )
                    })
                })
                .collect();

            for handle in handles {
                handle.join().expect("writer thread").expect("store cache");
            }

            let cache_path =
                cache_file_path(&super::cache_context_key(&context)).expect("cache path");
            assert!(
                cache_path.is_file(),
                "expected cache file after concurrent writers"
            );

            let cached = load_cached_workspace(&root, &context, false)
                .expect("read cache")
                .expect("cache hit after concurrent writers");
            assert_eq!(cached.apps.len(), 1);
        });
    }

    #[test]
    fn cached_apps_round_trip_metadata() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "nxr.category".to_owned(),
            JsonValue::String("ci".to_owned()),
        );
        let apps = vec![App {
            name: "test".to_owned(),
            attr_path: "apps.aarch64-darwin.test".to_owned(),
            flake_ref: root.as_str().to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: Some("run tests".to_owned()),
            is_default: true,
            metadata,
        }];

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("store cache");
            let cached = load_cached_workspace(&root, &context, false)
                .expect("read cache")
                .expect("cache hit");
            assert_eq!(cached.apps, apps);
        });
    }

    #[test]
    fn clear_and_status_report_discovery_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(&root, &context, &WorkspaceDiscovery { apps, tasks: None })
                .expect("store cache");

            let status = super::discovery_cache_status().expect("status");
            assert_eq!(status.entries, 1);
            assert!(status.total_bytes > 0);
            assert!(!status.path.is_empty());

            let removed = super::clear_discovery_cache().expect("clear");
            assert_eq!(removed, 1);

            let status = super::discovery_cache_status().expect("status after clear");
            assert_eq!(status.entries, 0);
            assert_eq!(status.total_bytes, 0);
        });
    }

    #[test]
    fn content_change_same_length_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        fs::write(root.join("flake.nix"), "{ a = 1; }").expect("flake.nix");
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            fs::write(root.join("flake.nix"), "{ a = 2; }").expect("edit flake.nix");

            assert!(
                load_cached_workspace(&root, &context, false)
                    .expect("read cache")
                    .is_none()
            );
        });
    }

    #[test]
    fn schema_version_mismatch_is_cache_miss() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            let path = cache_file_path(&super::cache_context_key(&context)).expect("cache path");
            let mut value: JsonValue =
                serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
            value["schema_version"] = JsonValue::from(2);
            fs::write(&path, serde_json::to_vec_pretty(&value).expect("serialize"))
                .expect("rewrite");

            assert!(
                load_cached_workspace(&root, &context, false)
                    .expect("read cache")
                    .is_none()
            );
        });
    }

    #[test]
    fn discovery_inputs_content_change_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"a\"\n").expect("cargo");
        let context = test_context(&root, "aarch64-darwin")
            .with_discovery_inputs(vec!["Cargo.toml".to_owned()]);
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            fs::write(root.join("Cargo.toml"), "[package]\nname = \"b\"\n").expect("edit cargo");

            assert!(
                load_cached_workspace(&root, &context, false)
                    .expect("read cache")
                    .is_none()
            );
        });
    }

    #[test]
    fn ttl_expiry_is_cache_miss() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_workspace(
                &root,
                &context,
                &WorkspaceDiscovery {
                    apps: apps.clone(),
                    tasks: None,
                },
            )
            .expect("seed cache");

            let path = cache_file_path(&super::cache_context_key(&context)).expect("cache path");
            let mut value: JsonValue =
                serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
            value["cached_at"] = JsonValue::from(1u64);
            fs::write(&path, serde_json::to_vec_pretty(&value).expect("serialize"))
                .expect("rewrite");

            super::TEST_CACHE_TTL_SECS.with(|cell| {
                *cell.borrow_mut() = Some(Some(60));
            });
            let miss = load_cached_workspace(&root, &context, false).expect("read cache");
            super::TEST_CACHE_TTL_SECS.with(|cell| {
                *cell.borrow_mut() = None;
            });

            assert!(miss.is_none());
        });
    }
}
