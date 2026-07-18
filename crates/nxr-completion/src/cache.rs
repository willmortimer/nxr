//! Discovery metadata cache keyed by local flake inputs.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use nxr_core::App;
use serde::{Deserialize, Serialize};

/// Inputs that identify a cached discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryContext {
    pub flake_ref: String,
    pub local_root: Option<Utf8PathBuf>,
    pub system: String,
}

/// Options controlling cache lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryCacheOptions {
    pub refresh: bool,
}

impl DiscoveryCacheOptions {
    #[must_use]
    pub const fn fresh() -> Self {
        Self { refresh: true }
    }

    #[must_use]
    pub const fn normal() -> Self {
        Self { refresh: false }
    }
}

/// Return cached apps when valid, otherwise run `discover` and update the cache.
///
/// Remote flakes (no `local_root`) always call `discover` directly. Cache read and
/// write failures are treated as cache misses or no-ops so discovery still succeeds.
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
    if options.refresh {
        let apps = discover()?;
        if let Some(local_root) = &context.local_root {
            let _ = store_cached_apps(local_root, context, &apps);
        }
        return Ok(apps);
    }

    let Some(local_root) = &context.local_root else {
        return discover();
    };

    if let Ok(Some(apps)) = load_cached_apps(local_root, context) {
        return Ok(apps);
    }

    let apps = discover()?;
    let _ = store_cached_apps(local_root, context, &apps);
    Ok(apps)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CachedDiscovery {
    schema_version: u32,
    flake_root: String,
    flake_nix_mtime: MtimeStamp,
    flake_lock_mtime: Option<MtimeStamp>,
    system: String,
    flake_ref: String,
    apps: Vec<App>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct MtimeStamp {
    secs: u64,
    nanos: u32,
}

impl MtimeStamp {
    fn from_system_time(time: SystemTime) -> Self {
        let duration = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        }
    }
}

const CACHE_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
thread_local! {
    static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn cache_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_CACHE_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("discovery"))
}

fn cache_file_path(context: &DiscoveryContext) -> Option<PathBuf> {
    let root = cache_root()?;
    Some(root.join(cache_file_name(context)))
}

fn cache_file_name(context: &DiscoveryContext) -> String {
    let mut hasher = DefaultHasher::new();
    context.local_root.hash(&mut hasher);
    context.system.hash(&mut hasher);
    context.flake_ref.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

fn load_cached_apps(
    local_root: &Utf8Path,
    context: &DiscoveryContext,
) -> io::Result<Option<Vec<App>>> {
    let path = cache_file_path(context)
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

    if cached.flake_root != local_root.as_str()
        || cached.system != context.system
        || cached.flake_ref != context.flake_ref
    {
        return Ok(None);
    }

    let current_nix_mtime = file_mtime(&local_root.join("flake.nix"))?;
    if cached.flake_nix_mtime != MtimeStamp::from_system_time(current_nix_mtime) {
        return Ok(None);
    }

    let lock_path = local_root.join("flake.lock");
    let current_lock_mtime = file_mtime_optional(&lock_path)?;
    if cached.flake_lock_mtime != current_lock_mtime.map(MtimeStamp::from_system_time) {
        return Ok(None);
    }

    Ok(Some(cached.apps))
}

fn store_cached_apps(
    local_root: &Utf8Path,
    context: &DiscoveryContext,
    apps: &[App],
) -> io::Result<()> {
    let path = cache_file_path(context)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let entry = CachedDiscovery {
        schema_version: CACHE_SCHEMA_VERSION,
        flake_root: local_root.as_str().to_owned(),
        flake_nix_mtime: MtimeStamp::from_system_time(file_mtime(&local_root.join("flake.nix"))?),
        flake_lock_mtime: file_mtime_optional(&local_root.join("flake.lock"))?
            .map(MtimeStamp::from_system_time),
        system: context.system.clone(),
        flake_ref: context.flake_ref.clone(),
        apps: apps.to_vec(),
    };

    let serialized = serde_json::to_vec_pretty(&entry)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    write_atomically(&path, &serialized)
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    let temp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache")
    ));

    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)?;
        file.lock_exclusive()?;
        file.write_all(contents)?;
        file.sync_all()?;
        file.unlock()?;
    }

    fs::rename(temp_path, path)
}

fn file_mtime(path: &Utf8Path) -> io::Result<SystemTime> {
    fs::metadata(path)?.modified()
}

fn file_mtime_optional(path: &Utf8Path) -> io::Result<Option<SystemTime>> {
    match fs::metadata(path) {
        Ok(metadata) => metadata.modified().map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use nxr_core::App;
    use serde_json::Value as JsonValue;
    use tempfile::TempDir;

    use super::{
        DiscoveryCacheOptions, DiscoveryContext, cache_file_path, discover_with_cache,
        load_cached_apps, store_cached_apps,
    };

    fn test_context(root: &camino::Utf8Path, system: &str) -> DiscoveryContext {
        DiscoveryContext {
            flake_ref: root.as_str().to_owned(),
            local_root: Some(root.to_path_buf()),
            system: system.to_owned(),
        }
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

    fn write_flake(root: &camino::Utf8Path) {
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");
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

            let cached = load_cached_apps(&root, &context)
                .expect("read cache")
                .expect("cache hit");
            assert_eq!(cached, apps);

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
    fn refresh_bypasses_cache_and_replaces_entry() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let initial = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_apps(&root, &context, &initial).expect("seed cache");

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

            let cached = load_cached_apps(&root, &context)
                .expect("read cache")
                .expect("cache entry");
            assert_eq!(cached, refreshed);
        });
    }

    #[test]
    fn flake_nix_mtime_change_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_apps(&root, &context, &apps).expect("seed cache");

            fs::write(root.join("flake.nix"), "{ changed = true; }").expect("rewrite flake.nix");

            let cached = load_cached_apps(&root, &context).expect("read cache");
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
    fn flake_lock_mtime_change_invalidates_cache() {
        let temp = TempDir::new().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        write_flake(&root);
        fs::write(root.join("flake.lock"), "{}").expect("write flake.lock");
        let context = test_context(&root, "aarch64-darwin");
        let apps = sample_apps(root.as_str(), "aarch64-darwin");

        with_cache_dir(&temp, || {
            store_cached_apps(&root, &context, &apps).expect("seed cache");

            fs::write(root.join("flake.lock"), "{ \"nodes\": {} }").expect("rewrite flake.lock");

            assert!(
                load_cached_apps(&root, &context)
                    .expect("read cache")
                    .is_none()
            );
        });
    }

    #[test]
    fn remote_flake_skips_cache() {
        let temp = TempDir::new().expect("tempdir");
        let context = DiscoveryContext {
            flake_ref: "github:owner/repo".to_owned(),
            local_root: None,
            system: "aarch64-darwin".to_owned(),
        };

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
            store_cached_apps(&root, &context, &apps).expect("store cache");
            let cached = load_cached_apps(&root, &context)
                .expect("read cache")
                .expect("cache hit");
            assert_eq!(cached, apps);
        });
    }
}
