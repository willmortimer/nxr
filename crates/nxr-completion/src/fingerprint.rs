//! Recursive `.nix` tree fingerprint for discovery cache invalidation.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use blake3::{Hash, Hasher};
use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use nxr_core::{normalize_repo_relative_path, validate_repo_relative_path};

/// Environment variable with colon-separated glob patterns excluded from fingerprinting.
///
/// Use this to skip huge vendored `.nix` trees that rarely affect discovery.
pub const FINGERPRINT_IGNORE_ENV: &str = "NXR_CACHE_FINGERPRINT_IGNORE";

const FINGERPRINT_INDEX_SCHEMA_VERSION: u32 = 1;
const DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION: u32 = 1;
const BUILTIN_IGNORE_POLICY_VERSION: &str = "v1";
/// Sentinel content hash for a declared discovery input that is absent on disk.
const MISSING_INPUT_CONTENT_HASH: &str =
    "000000000000000000000000000000000000000000000000000000006d697373";

/// Content digest of all `.nix` files under `flake_root` (hex-encoded BLAKE3).
///
/// Built-in directory ignores match common Nix/Rust artifacts (`.git`, `result`,
/// `target`, …). Additional subtrees may be excluded via [`FINGERPRINT_IGNORE_ENV`].
/// `flake.lock` content is included when present. Non-`.nix` sources are not hashed
/// here; declare extras via `perSystem.nxr.discoveryInputs`.
///
/// Uses a persisted per-file index so unchanged files are not re-read on warm paths.
///
/// # Errors
///
/// Returns an I/O error when the tree cannot be walked or a file cannot be read.
pub fn nix_tree_fingerprint(flake_root: &Utf8Path) -> io::Result<String> {
    let ignore = configured_ignore_globs()?;
    nix_tree_fingerprint_with_ignore(flake_root, &ignore)
}

/// Fingerprint helper for tests and callers supplying explicit ignore globs.
pub(crate) fn nix_tree_fingerprint_with_ignore(
    flake_root: &Utf8Path,
    extra_ignore: &GlobSet,
) -> io::Result<String> {
    let root = canonical_flake_root(flake_root);
    let ignore_policy_hash = ignore_policy_hash(extra_ignore);
    let index_path = fingerprint_index_path(&root);

    let loaded = match index_path.as_ref() {
        Some(path) => load_fingerprint_index(path)?,
        None => None,
    };

    let (fingerprint, index) =
        compute_workspace_fingerprint(&root, extra_ignore, &ignore_policy_hash, loaded.as_ref())?;

    if let Some(path) = index_path {
        // Skip pretty-JSON rewrite when the warm path produced an identical index.
        if loaded.as_ref() != Some(&index) {
            store_fingerprint_index(&path, &index)?;
        }
    }

    Ok(fingerprint)
}

/// Content-hash sorted flake-root-relative discovery input paths (hex BLAKE3).
///
/// Missing paths hash as an explicit absence marker so deletion invalidates the
/// cache. Paths are sorted and deduplicated before hashing. Absolute paths,
/// parent traversal, and symlink escapes outside the flake root are rejected.
///
/// Uses a persisted per-path index (same metadata gate as the Nix tree index)
/// so unchanged discovery inputs are not re-read on warm paths.
///
/// # Errors
///
/// Returns an I/O error when a path is invalid, escapes the flake root, or
/// exists but cannot be read.
pub fn discovery_inputs_fingerprint(
    flake_root: &Utf8Path,
    inputs: &[String],
) -> io::Result<String> {
    let root = canonical_flake_root(flake_root);
    let mut paths: Vec<&str> = inputs
        .iter()
        .map(String::as_str)
        .filter(|path| !path.is_empty())
        .collect();
    paths.sort_unstable();
    paths.dedup();

    let index_path = discovery_inputs_index_path(&root);
    let loaded = match index_path.as_ref() {
        Some(path) => load_discovery_inputs_index(path)?,
        None => None,
    };
    let prior = loaded
        .as_ref()
        .filter(|index| discovery_inputs_index_compatible(index, &root));

    let mut entries = BTreeMap::new();
    for relative in paths {
        validate_repo_relative_path("discoveryInputs", relative)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        let normalized = normalize_repo_relative_path(relative).to_owned();
        let prior_entry = prior.and_then(|index| index.entries.get(&normalized));
        let entry = fingerprint_discovery_input(&root, &normalized, prior_entry)?;
        entries.insert(normalized, entry);
    }

    let fingerprint = aggregate_discovery_inputs_fingerprint(&entries);
    let index = DiscoveryInputsIndex {
        schema_version: DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION,
        root: root.as_str().to_owned(),
        entries,
    };

    if let Some(path) = index_path
        && loaded.as_ref() != Some(&index)
    {
        store_discovery_inputs_index(&path, &index)?;
    }

    Ok(fingerprint)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct WorkspaceFingerprintIndex {
    schema_version: u32,
    root: String,
    ignore_policy_hash: String,
    entries: BTreeMap<String, FingerprintEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_file: Option<LockFingerprintEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FingerprintEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_identity: Option<FileIdentity>,
    size: u64,
    modified_ns: u128,
    content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct LockFingerprintEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_identity: Option<FileIdentity>,
    size: u64,
    modified_ns: u128,
    content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DiscoveryInputsIndex {
    schema_version: u32,
    root: String,
    entries: BTreeMap<String, FingerprintEntry>,
}

fn compute_workspace_fingerprint(
    root: &Utf8Path,
    extra_ignore: &GlobSet,
    ignore_policy_hash: &str,
    loaded: Option<&WorkspaceFingerprintIndex>,
) -> io::Result<(String, WorkspaceFingerprintIndex)> {
    let reuse_index =
        loaded.is_some_and(|index| index_is_compatible(index, root, ignore_policy_hash));

    let prior = if reuse_index { loaded } else { None };

    let mut entries = BTreeMap::new();
    let mut nix_paths = Vec::new();
    walk_nix_files(root, root, extra_ignore, &mut nix_paths)?;

    for relative in nix_paths {
        let absolute = root.join(&relative);
        let metadata = fs::metadata(&absolute)?;
        let prior_entry = prior.and_then(|index| index.entries.get(&relative));
        let entry = fingerprint_entry_for_file(&absolute, &metadata, prior_entry)?;
        entries.insert(relative, entry);
    }

    let lock_path = root.join("flake.lock");
    let lock_file = match fs::metadata(&lock_path) {
        Ok(metadata) if metadata.is_file() => {
            let prior_lock = prior.and_then(|index| index.lock_file.as_ref());
            Some(fingerprint_lock_entry(&lock_path, &metadata, prior_lock)?)
        }
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let fingerprint = aggregate_fingerprint(&entries, lock_file.as_ref());
    let index = WorkspaceFingerprintIndex {
        schema_version: FINGERPRINT_INDEX_SCHEMA_VERSION,
        root: root.as_str().to_owned(),
        ignore_policy_hash: ignore_policy_hash.to_owned(),
        entries,
        lock_file,
    };

    Ok((fingerprint, index))
}

fn index_is_compatible(
    index: &WorkspaceFingerprintIndex,
    root: &Utf8Path,
    ignore_hash: &str,
) -> bool {
    index.schema_version == FINGERPRINT_INDEX_SCHEMA_VERSION
        && index.root == root.as_str()
        && index.ignore_policy_hash == ignore_hash
}

fn aggregate_fingerprint(
    entries: &BTreeMap<String, FingerprintEntry>,
    lock_file: Option<&LockFingerprintEntry>,
) -> String {
    let mut hasher = Hasher::new();
    for (path, entry) in entries {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(entry.content_hash_bytes().as_ref());
    }
    if let Some(lock) = lock_file {
        hasher.update(b"flake.lock");
        hasher.update(&[0]);
        hasher.update(lock.content_hash_bytes().as_ref());
    }
    hasher.finalize().to_hex().to_string()
}

fn fingerprint_entry_for_file(
    path: &Utf8Path,
    metadata: &fs::Metadata,
    prior: Option<&FingerprintEntry>,
) -> io::Result<FingerprintEntry> {
    let file_identity = file_identity(metadata);
    let size = metadata.len();
    let modified_ns = modified_ns(metadata)?;

    if let Some(prior) = prior
        && prior.matches_metadata(file_identity, size, modified_ns)
    {
        return Ok(prior.clone());
    }

    let bytes = read_file_bytes(path)?;
    Ok(FingerprintEntry {
        file_identity,
        size,
        modified_ns,
        content_hash: hash_bytes(&bytes).to_hex().to_string(),
    })
}

fn fingerprint_lock_entry(
    path: &Utf8Path,
    metadata: &fs::Metadata,
    prior: Option<&LockFingerprintEntry>,
) -> io::Result<LockFingerprintEntry> {
    let file_identity = file_identity(metadata);
    let size = metadata.len();
    let modified_ns = modified_ns(metadata)?;

    if let Some(prior) = prior
        && prior.matches_metadata(file_identity, size, modified_ns)
    {
        return Ok(prior.clone());
    }

    let bytes = read_file_bytes(path)?;
    Ok(LockFingerprintEntry {
        file_identity,
        size,
        modified_ns,
        content_hash: hash_bytes(&bytes).to_hex().to_string(),
    })
}

impl FingerprintEntry {
    fn matches_metadata(
        &self,
        file_identity: Option<FileIdentity>,
        size: u64,
        modified_ns: u128,
    ) -> bool {
        self.file_identity == file_identity && self.size == size && self.modified_ns == modified_ns
    }

    fn content_hash_bytes(&self) -> [u8; 32] {
        decode_hash_hex(&self.content_hash).unwrap_or([0u8; 32])
    }
}

impl LockFingerprintEntry {
    fn matches_metadata(
        &self,
        file_identity: Option<FileIdentity>,
        size: u64,
        modified_ns: u128,
    ) -> bool {
        self.file_identity == file_identity && self.size == size && self.modified_ns == modified_ns
    }

    fn content_hash_bytes(&self) -> [u8; 32] {
        decode_hash_hex(&self.content_hash).unwrap_or([0u8; 32])
    }
}

fn decode_hash_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[index] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hash_bytes(bytes: &[u8]) -> Hash {
    blake3::hash(bytes)
}

// Returns `None` on non-Unix targets where durable file identity is unavailable.
#[allow(clippy::unnecessary_wraps)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn modified_ns(metadata: &fs::Metadata) -> io::Result<u128> {
    let modified = metadata.modified()?;
    let duration = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(duration.as_nanos())
}

fn ignore_policy_hash(extra_ignore: &GlobSet) -> String {
    let mut hasher = Hasher::new();
    hasher.update(BUILTIN_IGNORE_POLICY_VERSION.as_bytes());
    hasher.update(&[0]);
    if let Some(raw) = std::env::var_os(FINGERPRINT_IGNORE_ENV) {
        hasher.update(raw.as_encoded_bytes());
    }
    hasher.update(&[0]);
    hasher.update(&u8::from(extra_ignore.is_empty()).to_le_bytes());
    // GlobSet does not expose its patterns; env + emptiness still invalidates policy changes.
    hasher.finalize().to_hex().to_string()
}

fn fingerprint_index_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_FINGERPRINT_INDEX_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    #[cfg(test)]
    if let Ok(guard) = CONCURRENT_TEST_FINGERPRINT_INDEX_ROOT.lock()
        && let Some(root) = guard.clone()
    {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("discovery").join("fingerprint-index"))
}

fn fingerprint_index_path(root: &Utf8Path) -> Option<PathBuf> {
    let index_root = fingerprint_index_root()?;
    let mut hasher = Hasher::new();
    hasher.update(root.as_str().as_bytes());
    Some(index_root.join(format!("{}.json", hasher.finalize().to_hex())))
}

fn load_fingerprint_index(path: &Path) -> io::Result<Option<WorkspaceFingerprintIndex>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let index: WorkspaceFingerprintIndex = match serde_json::from_str(&contents) {
        Ok(index) => index,
        Err(_) => return Ok(None),
    };

    if index.schema_version != FINGERPRINT_INDEX_SCHEMA_VERSION {
        return Ok(None);
    }

    if index
        .entries
        .values()
        .any(|entry| decode_hash_hex(&entry.content_hash).is_none())
        || index
            .lock_file
            .as_ref()
            .is_some_and(|lock| decode_hash_hex(&lock.content_hash).is_none())
    {
        return Ok(None);
    }

    Ok(Some(index))
}

fn store_fingerprint_index(path: &Path, index: &WorkspaceFingerprintIndex) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;

    let serialized = serde_json::to_vec_pretty(index)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    record_index_store();
    write_atomically(path, &serialized)
}

fn discovery_inputs_index_path(root: &Utf8Path) -> Option<PathBuf> {
    let index_root = fingerprint_index_root()?
        .parent()?
        .join("discovery-inputs-index");
    let mut hasher = Hasher::new();
    hasher.update(root.as_str().as_bytes());
    Some(index_root.join(format!("{}.json", hasher.finalize().to_hex())))
}

fn discovery_inputs_index_compatible(index: &DiscoveryInputsIndex, root: &Utf8Path) -> bool {
    index.schema_version == DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION && index.root == root.as_str()
}

fn load_discovery_inputs_index(path: &Path) -> io::Result<Option<DiscoveryInputsIndex>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let index: DiscoveryInputsIndex = match serde_json::from_str(&contents) {
        Ok(index) => index,
        Err(_) => return Ok(None),
    };

    if index.schema_version != DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION {
        return Ok(None);
    }

    if index.entries.values().any(|entry| {
        entry.content_hash != MISSING_INPUT_CONTENT_HASH
            && decode_hash_hex(&entry.content_hash).is_none()
    }) {
        return Ok(None);
    }

    Ok(Some(index))
}

fn store_discovery_inputs_index(path: &Path, index: &DiscoveryInputsIndex) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;

    let serialized = serde_json::to_vec_pretty(index)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    record_index_store();
    write_atomically(path, &serialized)
}

fn aggregate_discovery_inputs_fingerprint(entries: &BTreeMap<String, FingerprintEntry>) -> String {
    let mut hasher = Hasher::new();
    for (path, entry) in entries {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(entry.content_hash_bytes().as_ref());
    }
    hasher.finalize().to_hex().to_string()
}

fn fingerprint_discovery_input(
    root: &Utf8Path,
    relative: &str,
    prior: Option<&FingerprintEntry>,
) -> io::Result<FingerprintEntry> {
    let joined = root.join(relative);
    let canonical = match joined.canonicalize_utf8() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if relative_escapes(relative) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("discovery input `{relative}` escapes flake root"),
                ));
            }
            return Ok(missing_discovery_input_entry());
        }
        Err(error) => return Err(error),
    };
    if !canonical.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("discovery input `{relative}` escapes flake root"),
        ));
    }
    let metadata = fs::metadata(&canonical)?;
    fingerprint_entry_for_file(&canonical, &metadata, prior)
}

fn missing_discovery_input_entry() -> FingerprintEntry {
    FingerprintEntry {
        file_identity: None,
        size: 0,
        modified_ns: 0,
        content_hash: MISSING_INPUT_CONTENT_HASH.to_owned(),
    }
}

fn record_index_store() {
    #[cfg(test)]
    INDEX_STORES.with(|counter| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;

    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("fingerprint-index")
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
            .unwrap_or("fingerprint-index"),
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
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

fn read_file_bytes(path: &Utf8Path) -> io::Result<Vec<u8>> {
    record_file_read();
    fs::read(path)
}

fn record_file_read() {
    #[cfg(test)]
    FILE_BYTES_READS.with(|counter| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
}

#[cfg(test)]
static CONCURRENT_TEST_FINGERPRINT_INDEX_ROOT: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
thread_local! {
    static TEST_FINGERPRINT_INDEX_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static FILE_BYTES_READS: std::sync::atomic::AtomicUsize =
        const { std::sync::atomic::AtomicUsize::new(0) };
    static INDEX_STORES: std::sync::atomic::AtomicUsize =
        const { std::sync::atomic::AtomicUsize::new(0) };
}

#[cfg(test)]
pub(crate) fn set_test_fingerprint_index_root(root: Option<PathBuf>) {
    TEST_FINGERPRINT_INDEX_ROOT.with(|cell| {
        *cell.borrow_mut() = root;
    });
}

#[cfg(test)]
pub(crate) fn set_concurrent_test_fingerprint_index_root(root: Option<PathBuf>) {
    let mut guard = CONCURRENT_TEST_FINGERPRINT_INDEX_ROOT
        .lock()
        .expect("concurrent fingerprint index lock");
    *guard = root;
}

#[cfg(test)]
fn reset_file_read_counter() -> usize {
    FILE_BYTES_READS.with(|counter| counter.swap(0, std::sync::atomic::Ordering::SeqCst))
}

#[cfg(test)]
fn reset_index_store_counter() -> usize {
    INDEX_STORES.with(|counter| counter.swap(0, std::sync::atomic::Ordering::SeqCst))
}

fn relative_escapes(relative: &str) -> bool {
    Path::new(relative)
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
}

fn configured_ignore_globs() -> io::Result<GlobSet> {
    let Some(raw) = std::env::var_os(FINGERPRINT_IGNORE_ENV) else {
        return Ok(GlobSet::empty());
    };

    let mut builder = GlobSetBuilder::new();
    for pattern in raw
        .to_string_lossy()
        .split(':')
        .filter(|part| !part.is_empty())
    {
        let glob = Glob::new(pattern).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {FINGERPRINT_IGNORE_ENV} glob `{pattern}`: {error}"),
            )
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn canonical_flake_root(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf())
}

fn walk_nix_files(
    root: &Utf8Path,
    dir: &Utf8Path,
    extra_ignore: &GlobSet,
    entries: &mut Vec<String>,
) -> io::Result<()> {
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            let Some(utf8_path) = Utf8Path::from_path(&path) else {
                continue;
            };

            if should_ignore(root, utf8_path, extra_ignore) {
                continue;
            }

            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(utf8_path.to_path_buf());
                continue;
            }

            if file_type.is_file() && is_nix_file(utf8_path) {
                let relative = utf8_path
                    .strip_prefix(root)
                    .unwrap_or(utf8_path)
                    .as_str()
                    .to_owned();
                entries.push(relative);
            }
        }
    }

    entries.sort();
    Ok(())
}

fn is_nix_file(path: &Utf8Path) -> bool {
    path.extension().is_some_and(|ext| ext == "nix")
}

fn should_ignore(root: &Utf8Path, path: &Utf8Path, extra_ignore: &GlobSet) -> bool {
    if is_builtin_ignored(path) {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = relative_for_glob(relative.as_str());
    extra_ignore.is_match(relative)
}

fn is_builtin_ignored(path: &Utf8Path) -> bool {
    if path.starts_with("/nix/store") {
        return true;
    }

    path.components().any(|component| {
        let name = component.as_str();
        name == ".git"
            || name == ".direnv"
            || name == ".cache"
            || name == "node_modules"
            || name == "target"
            || name == "result"
            || name.starts_with("result-")
    })
}

fn relative_for_glob(relative: &str) -> &str {
    relative.strip_prefix("./").unwrap_or(relative)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{
        discovery_inputs_fingerprint, nix_tree_fingerprint, nix_tree_fingerprint_with_ignore,
        reset_file_read_counter, reset_index_store_counter, set_test_fingerprint_index_root,
    };
    use globset::{Glob, GlobSetBuilder};

    fn utf8_root(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path")
    }

    fn with_fingerprint_index_dir<T>(temp: &TempDir, f: impl FnOnce() -> T) -> T {
        let index_root = temp.path().join("fingerprint-index");
        fs::create_dir_all(&index_root).expect("fingerprint index dir");
        set_test_fingerprint_index_root(Some(index_root));
        let result = f();
        set_test_fingerprint_index_root(None);
        result
    }

    #[test]
    fn imported_nix_change_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("write flake");
            fs::create_dir_all(root.join("nix")).expect("mkdir");
            fs::write(root.join("nix/apps.nix"), "1\n").expect("write apps");
            let initial = nix_tree_fingerprint(&root).expect("fingerprint");
            fs::write(root.join("nix/apps.nix"), "2\n").expect("edit apps");
            let updated = nix_tree_fingerprint(&root).expect("fingerprint after edit");
            assert_ne!(initial, updated);
        });
    }

    #[test]
    fn content_change_same_length_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("write flake");
            fs::write(root.join("a.nix"), "aaaa\n").expect("write");
            let initial = nix_tree_fingerprint(&root).expect("fingerprint");
            fs::write(root.join("a.nix"), "bbbb\n").expect("rewrite");
            let updated = nix_tree_fingerprint(&root).expect("fingerprint after edit");
            assert_ne!(initial, updated);
        });
    }

    #[test]
    fn incremental_index_rehashes_only_changed_file() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::create_dir_all(root.join("nix")).expect("mkdir");
            fs::write(root.join("nix/a.nix"), "a\n").expect("a");
            fs::write(root.join("nix/b.nix"), "b\n").expect("b");

            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("initial");
            assert_eq!(reset_file_read_counter(), 3);

            reset_file_read_counter();
            let baseline = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(
                reset_file_read_counter(),
                0,
                "unchanged warm path reads no files"
            );

            fs::write(root.join("nix/b.nix"), "changed\n").expect("edit b");
            reset_file_read_counter();
            let updated = nix_tree_fingerprint(&root).expect("after edit");
            assert_ne!(baseline, updated);
            assert_eq!(
                reset_file_read_counter(),
                1,
                "only the edited file should be re-read"
            );
        });
    }

    #[test]
    fn warm_load_skips_unchanged_file_bytes() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            for index in 0..8 {
                fs::write(root.join(format!("pkg-{index}.nix")), format!("{index}\n"))
                    .expect("write pkg");
            }

            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("cold");
            assert_eq!(reset_file_read_counter(), 9);

            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(reset_file_read_counter(), 0);
        });
    }

    #[test]
    fn warm_fingerprint_skips_unchanged_index_rewrite() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            reset_index_store_counter();
            let _ = nix_tree_fingerprint(&root).expect("cold");
            assert_eq!(reset_index_store_counter(), 1, "cold path stores index");

            let _ = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(
                reset_index_store_counter(),
                0,
                "unchanged warm path must not rewrite the index"
            );
        });
    }

    #[test]
    fn discovery_inputs_warm_path_skips_reread_and_rewrite() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("extra.txt"), "one\n").expect("extra");
            let inputs = vec!["extra.txt".to_owned()];

            reset_file_read_counter();
            reset_index_store_counter();
            let first = discovery_inputs_fingerprint(&root, &inputs).expect("cold");
            assert_eq!(reset_file_read_counter(), 1);
            assert_eq!(reset_index_store_counter(), 1);

            reset_file_read_counter();
            reset_index_store_counter();
            let second = discovery_inputs_fingerprint(&root, &inputs).expect("warm");
            assert_eq!(first, second);
            assert_eq!(reset_file_read_counter(), 0);
            assert_eq!(reset_index_store_counter(), 0);

            fs::write(root.join("extra.txt"), "two\n").expect("edit");
            reset_file_read_counter();
            let third = discovery_inputs_fingerprint(&root, &inputs).expect("after edit");
            assert_ne!(first, third);
            assert_eq!(reset_file_read_counter(), 1);
        });
    }

    #[test]
    fn synthetic_monorepo_warm_fingerprint_scales() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("flake");
            for index in 0..500 {
                let dir = root.join(format!("pkg{index}"));
                fs::create_dir_all(&dir).expect("mkdir");
                fs::write(dir.join("default.nix"), format!("{{ idx = {index}; }}\n"))
                    .expect("write nix");
            }

            reset_file_read_counter();
            reset_index_store_counter();
            let cold = nix_tree_fingerprint(&root).expect("cold");
            assert_eq!(reset_file_read_counter(), 501);
            assert_eq!(reset_index_store_counter(), 1);

            reset_file_read_counter();
            reset_index_store_counter();
            let warm = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(cold, warm);
            assert_eq!(
                reset_file_read_counter(),
                0,
                "500-file warm path should not re-read file bytes"
            );
            assert_eq!(
                reset_index_store_counter(),
                0,
                "500-file warm path should not rewrite the index"
            );
        });
    }

    #[test]
    fn corrupt_index_rebuilds_from_disk() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            let first = nix_tree_fingerprint(&root).expect("first");

            let index_root = temp.path().join("fingerprint-index");
            let index_file = fs::read_dir(&index_root)
                .expect("read index dir")
                .map(|entry| entry.expect("entry").path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("index json file");
            fs::write(&index_file, "{not-json").expect("corrupt index");

            reset_file_read_counter();
            let second = nix_tree_fingerprint(&root).expect("rebuild");
            assert_eq!(first, second);
            assert_eq!(reset_file_read_counter(), 1);
        });
    }

    #[test]
    fn fingerprint_ignore_env_skips_matching_subtree() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
            fs::create_dir_all(root.join("vendor")).expect("mkdir");
            fs::write(root.join("vendor/x.nix"), "1\n").expect("write vendor");
            let with_vendor = nix_tree_fingerprint(&root).expect("fingerprint with vendor");

            let mut builder = GlobSetBuilder::new();
            builder.add(Glob::new("vendor/**").expect("glob"));
            let ignore = builder.build().expect("build");
            let without_vendor = nix_tree_fingerprint_with_ignore(&root, &ignore)
                .expect("fingerprint ignoring vendor");
            assert_ne!(with_vendor, without_vendor);
        });
    }

    #[test]
    fn symlink_flake_root_matches_canonical_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
            let link = temp.path().join("link");
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(temp.path(), &link).expect("symlink");
            }
            #[cfg(not(unix))]
            {
                return;
            }
            let canonical = nix_tree_fingerprint(&root).expect("canonical fingerprint");
            let linked =
                nix_tree_fingerprint(&camino::Utf8PathBuf::from_path_buf(link).expect("utf8 link"))
                    .expect("symlink fingerprint");
            assert_eq!(canonical, linked);
        });
    }

    #[test]
    fn flake_lock_change_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
            let baseline = nix_tree_fingerprint(&root).expect("baseline");
            fs::write(root.join("flake.lock"), "{}\n").expect("write lock");
            let changed = nix_tree_fingerprint(&root).expect("changed");
            assert_ne!(baseline, changed);
        });
    }

    #[test]
    fn flake_lock_atomic_replace_changes_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
            fs::write(root.join("flake.lock"), "v1\n").expect("write lock");
            let initial = nix_tree_fingerprint(&root).expect("initial fingerprint");
            thread::sleep(Duration::from_millis(5));
            let lock_tmp = root.join("flake.lock.tmp");
            fs::write(&lock_tmp, "v2\n").expect("write tmp");
            fs::rename(&lock_tmp, root.join("flake.lock")).expect("rename");
            let updated = nix_tree_fingerprint(&root).expect("updated fingerprint");
            assert_ne!(initial, updated);
        });
    }

    #[test]
    fn discovery_input_content_change_invalidates() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("extra.txt"), "one\n").expect("write");
        let inputs = vec!["extra.txt".to_owned()];
        let initial = discovery_inputs_fingerprint(&root, &inputs).expect("initial");
        fs::write(root.join("extra.txt"), "two\n").expect("rewrite");
        let updated = discovery_inputs_fingerprint(&root, &inputs).expect("updated");
        assert_ne!(initial, updated);
    }

    #[test]
    fn discovery_input_rejects_parent_escape() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let err =
            discovery_inputs_fingerprint(&root, &["../escape".to_owned()]).expect_err("escape");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
