//! Recursive `.nix` tree fingerprint for discovery cache invalidation.
//!
//! Git checkouts fingerprint the `.nix` files Nix would copy for a Git flake:
//! tracked files, untracked files that are not gitignored, and paths named by
//! a Nix path literal in `flake.nix`. Independent root `wt-*` trees are skipped
//! unless `flake.nix` references them. `NXR_FINGERPRINT_FULL_WALK=1` restores
//! the full walk, including those trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

#[cfg(test)]
use std::cell::Cell;

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

/// When set to `1`, `true`, `yes`, or `on`, fingerprint the full tree.
///
/// This includes independent root `wt-*` directories and does not consult Git.
pub const FINGERPRINT_FULL_WALK_ENV: &str = "NXR_FINGERPRINT_FULL_WALK";

/// When set to `0`, `false`, `no`, or `off`, do not enumerate `.nix` paths from Git.
///
/// The recursive walk remains the candidate source. Independent `wt-*` trees are
/// still skipped unless [`FINGERPRINT_FULL_WALK_ENV`] is set.
pub const NIX_TREE_GIT_INDEX_ENV: &str = "NXR_NIX_TREE_GIT_INDEX";

/// When set to a positive integer, re-read every indexed file after that many seconds
/// even when metadata matches (backstop for same-mtime rewrites).
pub const FINGERPRINT_FORCE_REHASH_ENV: &str = "NXR_FINGERPRINT_FORCE_REHASH_SECS";

const FINGERPRINT_INDEX_SCHEMA_VERSION: u32 = 2;
const DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION: u32 = 2;
const LEGACY_FINGERPRINT_INDEX_SCHEMA_VERSION: u32 = 1;
const BUILTIN_IGNORE_POLICY_VERSION: &str = "v3";
const GIT_MODE_SYMLINK: u32 = 0o120000;
const GIT_MODE_GITLINK: u32 = 0o160000;
const SCAN_GIT: u8 = 1;
const SCAN_WALK: u8 = 2;
/// Sentinel content hash for a declared discovery input that is absent on disk.
const MISSING_INPUT_CONTENT_HASH: &str =
    "000000000000000000000000000000000000000000000000000000006d697373";

/// Content digest of all `.nix` files under `flake_root` (hex-encoded BLAKE3).
///
/// Built-in directory ignores match common Nix/Rust artifacts (`.git`, `result`,
/// `target`, …). Root directories named `wt-*` are also skipped unless `flake.nix`
/// references that relative path, or [`FINGERPRINT_FULL_WALK_ENV`] is set.
/// Additional subtrees may be excluded via [`FINGERPRINT_IGNORE_ENV`].
/// `flake.lock` content is included when present. Non-`.nix` sources are not hashed
/// here; declare extras via `perSystem.nxr.discoveryInputs`.
///
/// Uses a persisted per-file index so unchanged files are not re-read on warm paths.
/// Git checkouts take candidate paths from tracked files, untracked files that are
/// not gitignored, and Nix path literals in `flake.nix`.
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
    nix_tree_fingerprint_scanning(flake_root, extra_ignore, production_scan_mode())
}

fn nix_tree_fingerprint_scanning(
    flake_root: &Utf8Path,
    extra_ignore: &GlobSet,
    scan: ScanMode,
) -> io::Result<String> {
    let root = canonical_flake_root(flake_root);
    let ignore_policy_hash = ignore_policy_hash(extra_ignore, scan == ScanMode::FullWalk);
    let index_path = fingerprint_index_path(&root);

    let loaded = match index_path.as_ref() {
        Some(path) => load_fingerprint_index(path)?,
        None => None,
    };

    let (fingerprint, index) = compute_workspace_fingerprint(
        &root,
        extra_ignore,
        &ignore_policy_hash,
        loaded.as_ref(),
        scan,
    )?;

    if let Some(path) = index_path {
        // Skip rewrite when the warm path produced an identical index.
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
    let compatible = loaded
        .as_ref()
        .filter(|index| discovery_inputs_index_compatible(index, &root));
    let force_rehash = compatible.is_some_and(|index| should_force_rehash(index.last_rehash_ns));
    let prior = if force_rehash { None } else { compatible };
    let last_rehash_ns = resolve_last_rehash_ns(prior, force_rehash);

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
        last_rehash_ns,
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
    #[serde(default)]
    last_rehash_ns: u128,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_ns: Option<u128>,
    content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct LockFingerprintEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_identity: Option<FileIdentity>,
    size: u64,
    modified_ns: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_ns: Option<u128>,
    content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DiscoveryInputsIndex {
    schema_version: u32,
    root: String,
    entries: BTreeMap<String, FingerprintEntry>,
    #[serde(default)]
    last_rehash_ns: u128,
}

fn compute_workspace_fingerprint(
    root: &Utf8Path,
    extra_ignore: &GlobSet,
    ignore_policy_hash: &str,
    loaded: Option<&WorkspaceFingerprintIndex>,
    scan: ScanMode,
) -> io::Result<(String, WorkspaceFingerprintIndex)> {
    let compatible = loaded.filter(|index| index_is_compatible(index, root, ignore_policy_hash));
    let force_rehash = compatible.is_some_and(|index| should_force_rehash(index.last_rehash_ns));
    let prior = if force_rehash { None } else { compatible };
    let last_rehash_ns = resolve_last_rehash_ns(prior, force_rehash);

    let mut entries = BTreeMap::new();
    let nix_paths = collect_nix_paths(root, extra_ignore, scan)?;

    for relative in nix_paths {
        let absolute = root.join(&relative);
        let metadata = fs::metadata(&absolute)?;
        nxr_core::record_fs_metadata();
        let prior_entry = prior.and_then(|index| index.entries.get(&relative));
        let entry = fingerprint_entry_for_file(&absolute, &metadata, prior_entry)?;
        entries.insert(relative, entry);
    }

    let lock_path = root.join("flake.lock");
    let lock_file = match fs::metadata(&lock_path) {
        Ok(metadata) if metadata.is_file() => {
            nxr_core::record_fs_metadata();
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
        last_rehash_ns,
    };

    Ok((fingerprint, index))
}

fn index_is_compatible(
    index: &WorkspaceFingerprintIndex,
    root: &Utf8Path,
    ignore_hash: &str,
) -> bool {
    fingerprint_index_schema_supported(index.schema_version)
        && index.root == root.as_str()
        && index.ignore_policy_hash == ignore_hash
}

fn fingerprint_index_schema_supported(version: u32) -> bool {
    (LEGACY_FINGERPRINT_INDEX_SCHEMA_VERSION..=FINGERPRINT_INDEX_SCHEMA_VERSION).contains(&version)
}

fn discovery_inputs_index_schema_supported(version: u32) -> bool {
    (LEGACY_FINGERPRINT_INDEX_SCHEMA_VERSION..=DISCOVERY_INPUTS_INDEX_SCHEMA_VERSION)
        .contains(&version)
}

fn force_rehash_interval_secs() -> Option<u64> {
    #[cfg(test)]
    if let Ok(guard) = TEST_FORCE_REHASH_SECS.lock()
        && let Some(secs) = *guard
    {
        return Some(secs);
    }

    std::env::var(FINGERPRINT_FORCE_REHASH_ENV)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|&secs| secs > 0)
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn should_force_rehash(last_rehash_ns: u128) -> bool {
    let Some(interval_secs) = force_rehash_interval_secs() else {
        return false;
    };
    now_ns().saturating_sub(last_rehash_ns) >= u128::from(interval_secs) * 1_000_000_000
}

fn resolve_last_rehash_ns<T>(prior: Option<&T>, force_rehash: bool) -> u128
where
    T: LastRehashNs,
{
    if force_rehash {
        now_ns()
    } else if let Some(prior) = prior {
        prior.last_rehash_ns()
    } else {
        now_ns()
    }
}

trait LastRehashNs {
    fn last_rehash_ns(&self) -> u128;
}

impl LastRehashNs for WorkspaceFingerprintIndex {
    fn last_rehash_ns(&self) -> u128 {
        self.last_rehash_ns
    }
}

impl LastRehashNs for DiscoveryInputsIndex {
    fn last_rehash_ns(&self) -> u128 {
        self.last_rehash_ns
    }
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
    let changed_ns = changed_ns(metadata);

    if let Some(prior) = prior
        && prior.matches_metadata(file_identity, size, modified_ns, changed_ns)
    {
        return Ok(prior.clone());
    }

    let bytes = read_file_bytes(path)?;
    Ok(FingerprintEntry {
        file_identity,
        size,
        modified_ns,
        changed_ns,
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
    let changed_ns = changed_ns(metadata);

    if let Some(prior) = prior
        && prior.matches_metadata(file_identity, size, modified_ns, changed_ns)
    {
        return Ok(prior.clone());
    }

    let bytes = read_file_bytes(path)?;
    Ok(LockFingerprintEntry {
        file_identity,
        size,
        modified_ns,
        changed_ns,
        content_hash: hash_bytes(&bytes).to_hex().to_string(),
    })
}

impl FingerprintEntry {
    fn matches_metadata(
        &self,
        file_identity: Option<FileIdentity>,
        size: u64,
        modified_ns: u128,
        changed_ns: Option<u128>,
    ) -> bool {
        self.file_identity == file_identity
            && self.size == size
            && self.modified_ns == modified_ns
            && ctime_matches(self.changed_ns, changed_ns)
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
        changed_ns: Option<u128>,
    ) -> bool {
        self.file_identity == file_identity
            && self.size == size
            && self.modified_ns == modified_ns
            && ctime_matches(self.changed_ns, changed_ns)
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
    nxr_core::add_bytes_hashed(bytes.len() as u64);
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
    Ok(system_time_ns(metadata.modified()?))
}

fn changed_ns(metadata: &fs::Metadata) -> Option<u128> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let nanos =
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
        u128::try_from(nanos).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn system_time_ns(time: SystemTime) -> u128 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn ctime_matches(stored: Option<u128>, current: Option<u128>) -> bool {
    match (stored, current) {
        (Some(stored), Some(current)) => stored == current,
        _ => true,
    }
}

fn ignore_policy_hash(extra_ignore: &GlobSet, full_walk: bool) -> String {
    let mut hasher = Hasher::new();
    hasher.update(BUILTIN_IGNORE_POLICY_VERSION.as_bytes());
    hasher.update(&[0]);
    if let Some(raw) = std::env::var_os(FINGERPRINT_IGNORE_ENV) {
        hasher.update(raw.as_encoded_bytes());
    }
    hasher.update(&[0]);
    hasher.update(&[u8::from(full_walk)]);
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

    if !fingerprint_index_schema_supported(index.schema_version) {
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

    let serialized = serde_json::to_vec(index)
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
    discovery_inputs_index_schema_supported(index.schema_version) && index.root == root.as_str()
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

    if !discovery_inputs_index_schema_supported(index.schema_version) {
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

    let serialized = serde_json::to_vec(index)
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
    nxr_core::record_fs_metadata();
    fingerprint_entry_for_file(&canonical, &metadata, prior)
}

fn missing_discovery_input_entry() -> FingerprintEntry {
    FingerprintEntry {
        file_identity: None,
        size: 0,
        modified_ns: 0,
        changed_ns: None,
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
static TEST_FORCE_REHASH_SECS: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

#[cfg(test)]
thread_local! {
    static TEST_FINGERPRINT_INDEX_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static FILE_BYTES_READS: std::sync::atomic::AtomicUsize =
        const { std::sync::atomic::AtomicUsize::new(0) };
    static INDEX_STORES: std::sync::atomic::AtomicUsize =
        const { std::sync::atomic::AtomicUsize::new(0) };
    static NIX_SCAN: Cell<u8> = const { Cell::new(0) };
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
fn set_test_force_rehash_secs(secs: Option<u64>) {
    let mut guard = TEST_FORCE_REHASH_SECS
        .lock()
        .expect("test force rehash lock");
    *guard = secs;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanMode {
    /// Git-visible `.nix` files, otherwise the walk.
    Auto,
    /// Recursive walk, still skipping independent `wt-*` trees.
    Walk,
    /// Recursive walk including independent `wt-*` trees.
    FullWalk,
}

struct WalkFilter<'a> {
    extra_ignore: &'a GlobSet,
    full_walk: bool,
    references: &'a [String],
}

fn production_scan_mode() -> ScanMode {
    if env_is_truthy(FINGERPRINT_FULL_WALK_ENV) {
        ScanMode::FullWalk
    } else if env_flag_enabled(NIX_TREE_GIT_INDEX_ENV, true) {
        ScanMode::Auto
    } else {
        ScanMode::Walk
    }
}

fn env_is_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| parse_truthy(value.trim()))
}

fn parse_truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn env_flag_enabled(name: &str, default_on: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => parse_enabled_flag(value.trim(), default_on),
        Err(_) => default_on,
    }
}

fn parse_enabled_flag(value: &str, default_on: bool) -> bool {
    if value.is_empty() {
        return default_on;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn record_nix_scan(kind: u8) {
    #[cfg(test)]
    NIX_SCAN.with(|cell| cell.set(kind));
    #[cfg(not(test))]
    let _ = kind;
}

#[cfg(test)]
fn recorded_nix_scan() -> u8 {
    NIX_SCAN.with(|cell| cell.get())
}

fn collect_nix_paths(
    root: &Utf8Path,
    extra_ignore: &GlobSet,
    scan: ScanMode,
) -> io::Result<Vec<String>> {
    Ok(resolve_nix_paths(root, extra_ignore, scan)?.paths)
}

struct ResolvedPaths {
    paths: Vec<String>,
    tracked: Vec<String>,
    references: Vec<String>,
    used_git: bool,
    fallback_reason: Option<String>,
}

fn resolve_nix_paths(
    root: &Utf8Path,
    extra_ignore: &GlobSet,
    scan: ScanMode,
) -> io::Result<ResolvedPaths> {
    let flake_nix = read_flake_nix_text(root)?;
    let source = analyze_flake_source(&flake_nix);
    let references = source.references.clone();
    let filter = WalkFilter {
        extra_ignore,
        full_walk: scan == ScanMode::FullWalk,
        references: &references,
    };
    let mut entries = Vec::new();
    let mut tracked = Vec::new();
    let mut used_git = false;
    let mut fallback_reason = None;
    match scan {
        ScanMode::Auto => {
            if let Some(reason) = source.fallback {
                record_nix_scan(SCAN_WALK);
                fallback_reason = Some(reason.to_owned());
                walk_nix_files(root, root, &filter, &mut entries)?;
            } else {
                match git_visible_paths(root, &filter, &mut tracked)? {
                    Some(paths) => {
                        record_nix_scan(SCAN_GIT);
                        used_git = true;
                        entries = paths;
                    }
                    None => {
                        record_nix_scan(SCAN_WALK);
                        fallback_reason =
                            Some("git command failed or the checkout is not a worktree".to_owned());
                        walk_nix_files(root, root, &filter, &mut entries)?;
                    }
                }
            }
        }
        ScanMode::Walk => {
            record_nix_scan(SCAN_WALK);
            fallback_reason = Some("git index enumeration is disabled".to_owned());
            walk_nix_files(root, root, &filter, &mut entries)?;
        }
        ScanMode::FullWalk => {
            record_nix_scan(SCAN_WALK);
            fallback_reason = Some("full filesystem walk requested".to_owned());
            walk_nix_files(root, root, &filter, &mut entries)?;
        }
    }
    entries.sort();
    entries.dedup();
    tracked.sort();
    tracked.dedup();
    Ok(ResolvedPaths {
        paths: entries,
        tracked,
        references,
        used_git,
        fallback_reason,
    })
}

/// Why the Git-index fingerprint path was accepted or rejected.
///
/// This compares Git-visible `.nix` files with the compatibility walk. It is
/// for `nxr cache fingerprint explain` and is not part of `nxr list` or `nxr plan`.
#[derive(Clone, Debug, Serialize)]
pub struct FingerprintExplainReport {
    /// Git enumeration produced the candidate set.
    pub git_index_eligible: bool,
    /// The fingerprint used that candidate set instead of the recursive walk.
    pub fast_path_accepted: bool,
    /// Why the recursive walk was required, when it was.
    pub fallback_reason: Option<String>,
    /// Tracked `.nix` files from the Git index.
    pub tracked_nix: Vec<String>,
    /// `.nix` files the compatibility walk sees and Git visibility does not.
    pub additional_filesystem_nix: Vec<String>,
    /// Relative paths named by Nix path literals in `flake.nix`.
    pub explicit_references: Vec<String>,
    /// Directories entered by the compatibility walk.
    pub directories_traversed: u64,
    /// Files the compatibility walk visited.
    pub files_visited: u64,
    /// Largest immediate subtrees in that walk.
    pub top_subtrees: Vec<SubtreeCost>,
    /// Time to compute the production fingerprint, excluding this diagnostic walk.
    pub fingerprint_duration_ms: u128,
    /// Production fingerprint.
    pub fingerprint: String,
    /// The production fingerprint was computed.
    pub fingerprint_valid: bool,
}

/// One immediate subtree from the compatibility walk.
#[derive(Clone, Debug, Serialize)]
pub struct SubtreeCost {
    pub path: String,
    pub directories: u64,
    pub files: u64,
}

/// Explain fingerprint candidate selection for `flake_root`.
///
/// # Errors
///
/// Returns an I/O error when the tree cannot be read.
pub fn explain_nix_tree_fingerprint(flake_root: &Utf8Path) -> io::Result<FingerprintExplainReport> {
    let started = Instant::now();
    let fingerprint = nix_tree_fingerprint(flake_root)?;
    let fingerprint_duration_ms = started.elapsed().as_millis();
    let ignore = configured_ignore_globs()?;
    let resolved = resolve_nix_paths(flake_root, &ignore, production_scan_mode())?;
    let filter = WalkFilter {
        extra_ignore: &ignore,
        full_walk: false,
        references: &resolved.references,
    };
    let stats = compatibility_walk_stats(flake_root, &filter)?;
    let visible: BTreeSet<_> = resolved.paths.iter().cloned().collect();
    let additional_filesystem_nix = stats
        .nix
        .into_iter()
        .filter(|path| !visible.contains(path))
        .collect();
    Ok(FingerprintExplainReport {
        git_index_eligible: resolved.used_git,
        fast_path_accepted: resolved.used_git,
        fallback_reason: resolved.fallback_reason,
        tracked_nix: resolved.tracked,
        additional_filesystem_nix,
        explicit_references: resolved.references,
        directories_traversed: stats.directories,
        files_visited: stats.files,
        top_subtrees: stats.top,
        fingerprint_duration_ms,
        fingerprint,
        fingerprint_valid: true,
    })
}

struct CompatibilityWalk {
    directories: u64,
    files: u64,
    nix: Vec<String>,
    top: Vec<SubtreeCost>,
}

fn compatibility_walk_stats(
    root: &Utf8Path,
    filter: &WalkFilter<'_>,
) -> io::Result<CompatibilityWalk> {
    let mut directories = 0u64;
    let mut files = 0u64;
    let mut nix = Vec::new();
    let mut subtree_dirs: BTreeMap<String, u64> = BTreeMap::new();
    let mut subtree_files: BTreeMap<String, u64> = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        directories += 1;
        if let Some(top) = subtree_key(root, &current) {
            *subtree_dirs.entry(top).or_default() += 1;
        }
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            let Some(utf8_path) = Utf8Path::from_path(&path) else {
                continue;
            };
            if should_ignore(root, utf8_path, filter) {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(utf8_path.to_path_buf());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            files += 1;
            if let Some(top) = subtree_key(root, utf8_path) {
                *subtree_files.entry(top).or_default() += 1;
            }
            if is_nix_file(utf8_path) {
                let relative = utf8_path
                    .strip_prefix(root)
                    .unwrap_or(utf8_path)
                    .as_str()
                    .to_owned();
                nix.push(relative);
            }
        }
    }
    nix.sort();
    let mut top: Vec<SubtreeCost> = subtree_files
        .into_iter()
        .map(|(path, files)| SubtreeCost {
            directories: subtree_dirs.get(&path).copied().unwrap_or(0),
            path,
            files,
        })
        .collect();
    top.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then(left.path.cmp(&right.path))
    });
    top.truncate(8);
    Ok(CompatibilityWalk {
        directories,
        files,
        nix,
        top,
    })
}

fn subtree_key(root: &Utf8Path, path: &Utf8Path) -> Option<String> {
    use camino::Utf8Component;
    let relative = path.strip_prefix(root).ok()?;
    match relative.components().next()? {
        Utf8Component::Normal(name) => Some(name.to_owned()),
        _ => None,
    }
}

fn read_flake_nix_text(root: &Utf8Path) -> io::Result<String> {
    match fs::read_to_string(root.join("flake.nix")) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

struct SourceAnalysis {
    references: Vec<String>,
    fallback: Option<&'static str>,
}

fn analyze_flake_source(text: &str) -> SourceAnalysis {
    let mut references = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut stack = vec![SourceFrame::Code { braces: 0 }];
    while let Some((index, ch)) = chars.next() {
        match stack.last().copied() {
            Some(SourceFrame::LineComment) => {
                if ch == '\n' {
                    stack.pop();
                }
            }
            Some(SourceFrame::String) => {
                if ch == '\\' {
                    chars.next();
                } else if ch == '"' {
                    stack.pop();
                } else if ch == '$' && chars.peek().is_some_and(|(_, next)| *next == '{') {
                    chars.next();
                    stack.push(SourceFrame::Code { braces: 0 });
                }
            }
            Some(SourceFrame::Indented) => {
                if ch == '\'' && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    if chars
                        .peek()
                        .is_some_and(|(_, next)| matches!(*next, '\'' | '$' | '\\'))
                    {
                        chars.next();
                    } else {
                        stack.pop();
                    }
                } else if ch == '$' && chars.peek().is_some_and(|(_, next)| *next == '{') {
                    chars.next();
                    stack.push(SourceFrame::Code { braces: 0 });
                }
            }
            Some(SourceFrame::Code { braces }) => {
                if ch == '#' {
                    stack.push(SourceFrame::LineComment);
                    continue;
                }
                if ch == '"' {
                    stack.push(SourceFrame::String);
                    continue;
                }
                if ch == '\'' && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    stack.push(SourceFrame::Indented);
                    continue;
                }
                if ch == '{' {
                    if let Some(SourceFrame::Code { braces }) = stack.last_mut() {
                        *braces += 1;
                    }
                    continue;
                }
                if ch == '}' {
                    if braces > 0 {
                        if let Some(SourceFrame::Code { braces }) = stack.last_mut() {
                            *braces -= 1;
                        }
                    } else if stack.len() > 1 {
                        stack.pop();
                    }
                    continue;
                }
                let rest = &text[index..];
                if let Some(reason) = unresolvable_at(rest) {
                    return SourceAnalysis {
                        references,
                        fallback: Some(reason),
                    };
                }
                if let Some(path) = take_relative_path(rest) {
                    references.push(path);
                }
            }
            None => break,
        }
    }
    references.sort();
    references.dedup();
    SourceAnalysis {
        references,
        fallback: None,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SourceFrame {
    Code { braces: u32 },
    LineComment,
    String,
    Indented,
}

fn unresolvable_at(rest: &str) -> Option<&'static str> {
    if rest.starts_with("import \"${") || rest.starts_with("import '${") {
        return Some("unresolvable import");
    }
    if rest.starts_with("import (") {
        return Some("computed import");
    }
    if rest.starts_with("../") {
        return Some("path escapes the flake root");
    }
    if let Some(stripped) = rest.strip_prefix("builtins.readDir") {
        let after = stripped.trim_start();
        if !after.starts_with("./") && !after.starts_with("path:./") {
            return Some("builtins.readDir");
        }
    }
    if rest.starts_with("builtins.path") {
        let window = rest.chars().take(500).collect::<String>();
        if !path_binding_is_literal(&window) {
            return Some("builtins.path");
        }
    }
    None
}

fn path_binding_is_literal(window: &str) -> bool {
    let mut rest = window;
    while let Some(index) = rest.find("path") {
        let after = rest[index + "path".len()..].trim_start();
        if let Some(value) = after.strip_prefix('=') {
            let value = value.trim_start();
            return value.starts_with("./") || value.starts_with("path:./");
        }
        rest = &rest[index + "path".len()..];
    }
    false
}

fn take_relative_path(rest: &str) -> Option<String> {
    let body = if let Some(stripped) = rest.strip_prefix("path:./") {
        stripped
    } else {
        rest.strip_prefix("./")?
    };
    if body.starts_with('/') || body.starts_with("./") {
        return None;
    }
    let path: String = body
        .chars()
        .take_while(|ch| is_path_name_char(*ch) || *ch == '/')
        .collect();
    let path = path.trim_matches('/').to_owned();
    if path.is_empty() || path.split('/').any(|part| part.is_empty() || part == "..") {
        None
    } else {
        Some(path)
    }
}

fn git_visible_paths(
    root: &Utf8Path,
    filter: &WalkFilter<'_>,
    tracked: &mut Vec<String>,
) -> io::Result<Option<Vec<String>>> {
    let Some(inside) = git_stdout(root, &["rev-parse", "--is-inside-work-tree"])? else {
        return Ok(None);
    };
    if std::str::from_utf8(&inside)
        .ok()
        .is_none_or(|text| text.trim() != "true")
    {
        return Ok(None);
    }

    let Some(stage) = git_stdout(root, &["ls-files", "-z", "--stage", "--", "."])? else {
        return Ok(None);
    };
    let Some(untracked) = git_stdout(
        root,
        &[
            "ls-files",
            "-z",
            "-o",
            "--directory",
            "--exclude-standard",
            "--",
            ".",
        ],
    )?
    else {
        return Ok(None);
    };

    let mut paths = Vec::new();
    if !append_stage_nix(root, "", &stage, filter, tracked, &mut paths)? {
        return Ok(None);
    }
    if !append_worktree_nix(root, "", &untracked, filter, &mut paths, true)? {
        return Ok(None);
    }
    include_explicit_references(root, filter, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(Some(paths))
}

fn include_explicit_references(
    root: &Utf8Path,
    filter: &WalkFilter<'_>,
    paths: &mut Vec<String>,
) -> io::Result<()> {
    for reference in filter.references {
        let absolute = root.join(reference);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk_nix_files(root, &absolute, filter, paths)?;
            continue;
        }
        if metadata.is_file() && is_nix_name(reference) && !should_ignore(root, &absolute, filter) {
            paths.push(reference.clone());
        }
    }
    Ok(())
}

fn append_stage_nix(
    root: &Utf8Path,
    prefix: &str,
    stage: &[u8],
    filter: &WalkFilter<'_>,
    tracked: &mut Vec<String>,
    paths: &mut Vec<String>,
) -> io::Result<bool> {
    for record in nul_records(stage) {
        let Some(entry) = parse_stage_record(record) else {
            return Ok(false);
        };
        if entry.stage == 0 && entry.mode == GIT_MODE_GITLINK {
            continue;
        }
        if entry.stage == 0 && entry.mode == GIT_MODE_SYMLINK {
            continue;
        }
        if !is_nix_name(&entry.path) {
            continue;
        }
        let Some(relative) = prefixed_repo_path(prefix, &entry.path) else {
            return Ok(false);
        };
        let absolute = root.join(&relative);
        if should_ignore(root, &absolute, filter) {
            continue;
        }
        let before = paths.len();
        push_regular_nix_file(&absolute, &relative, paths)?;
        if paths.len() > before {
            tracked.push(relative);
        }
    }
    Ok(true)
}

fn append_worktree_nix(
    root: &Utf8Path,
    prefix: &str,
    listing: &[u8],
    filter: &WalkFilter<'_>,
    paths: &mut Vec<String>,
    walk_directories: bool,
) -> io::Result<bool> {
    for record in nul_records(listing) {
        let Ok(raw) = std::str::from_utf8(record) else {
            return Ok(false);
        };
        let Some(relative) = prefixed_repo_path(prefix, raw) else {
            return Ok(false);
        };
        let absolute = root.join(&relative);
        if should_ignore(root, &absolute, filter) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if walk_directories {
                walk_nix_files(root, &absolute, filter, paths)?;
            }
            continue;
        }
        if metadata.is_file() && is_nix_name(raw) {
            paths.push(relative);
        }
    }
    Ok(true)
}

fn push_regular_nix_file(
    absolute: &Utf8Path,
    relative: &str,
    paths: &mut Vec<String>,
) -> io::Result<()> {
    match fs::symlink_metadata(absolute) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(()),
        Ok(metadata) if metadata.is_file() => {
            paths.push(relative.to_owned());
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn parse_stage_record(record: &[u8]) -> Option<StageRecord> {
    let tab = record.iter().position(|byte| *byte == b'\t')?;
    let meta = std::str::from_utf8(&record[..tab]).ok()?;
    let path = std::str::from_utf8(&record[tab + 1..]).ok()?.to_owned();
    if path.is_empty() || relative_escapes(&path) {
        return None;
    }
    let mut parts = meta.split_whitespace();
    let mode = u32::from_str_radix(parts.next()?, 8).ok()?;
    let _oid = parts.next()?;
    let stage = parts.next()?.parse().ok()?;
    Some(StageRecord { mode, stage, path })
}

struct StageRecord {
    mode: u32,
    stage: u8,
    path: String,
}

fn prefixed_repo_path(prefix: &str, raw: &str) -> Option<String> {
    let relative = raw.trim_end_matches('/');
    if relative.is_empty() || relative_escapes(relative) {
        return None;
    }
    let joined = if prefix.is_empty() {
        relative.to_owned()
    } else {
        format!("{prefix}/{relative}")
    };
    if relative_escapes(&joined) {
        None
    } else {
        Some(joined)
    }
}

fn nul_records(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
}

fn git_stdout(dir: &Utf8Path, args: &[&str]) -> io::Result<Option<Vec<u8>>> {
    let output = match Command::new("git")
        .arg("-C")
        .arg(dir.as_str())
        .args(args)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if output.status.success() {
        Ok(Some(output.stdout))
    } else {
        Ok(None)
    }
}

fn walk_nix_files(
    root: &Utf8Path,
    dir: &Utf8Path,
    filter: &WalkFilter<'_>,
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

            if should_ignore(root, utf8_path, filter) {
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

fn should_ignore(root: &Utf8Path, path: &Utf8Path, filter: &WalkFilter<'_>) -> bool {
    if is_builtin_ignored(path) {
        return true;
    }
    if !filter.full_walk && is_independent_worktree(root, path, filter.references) {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = relative_for_glob(relative.as_str());
    filter.extra_ignore.is_match(relative)
}

fn is_independent_worktree(root: &Utf8Path, path: &Utf8Path, references: &[String]) -> bool {
    use camino::Utf8Component;

    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    let Some(Utf8Component::Normal(name)) = components.next() else {
        return false;
    };
    if !name.starts_with("wt-") {
        return false;
    }
    let nested = components.next().is_some();
    if !nested {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    !references.iter().any(|reference| {
        reference == name
            || reference
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

#[cfg(test)]
fn flake_references_worktree(flake_nix: &str, name: &str) -> bool {
    for needle in [format!("./{name}"), format!("path:./{name}")] {
        let mut rest = flake_nix;
        while let Some(index) = rest.find(&needle) {
            let after = index + needle.len();
            let boundary = rest[after..]
                .chars()
                .next()
                .is_none_or(|ch| !is_path_name_char(ch));
            if boundary {
                return true;
            }
            rest = &rest[index + needle.len()..];
        }
    }
    false
}

fn is_path_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn is_nix_name(path: &str) -> bool {
    Utf8Path::new(path.trim_end_matches('/'))
        .extension()
        .is_some_and(|ext| ext == "nix")
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

    use std::process::Command;

    use globset::{Glob, GlobSet, GlobSetBuilder};

    use super::{
        SCAN_GIT, SCAN_WALK, ScanMode, WalkFilter, analyze_flake_source, append_stage_nix,
        discovery_inputs_fingerprint, explain_nix_tree_fingerprint, flake_references_worktree,
        nix_tree_fingerprint, nix_tree_fingerprint_scanning, nix_tree_fingerprint_with_ignore,
        parse_enabled_flag, parse_truthy, production_scan_mode, recorded_nix_scan,
        reset_file_read_counter, reset_index_store_counter, set_test_fingerprint_index_root,
    };

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
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("extra.txt"), "one\n").expect("write");
            let inputs = vec!["extra.txt".to_owned()];
            let initial = discovery_inputs_fingerprint(&root, &inputs).expect("initial");
            fs::write(root.join("extra.txt"), "two\n").expect("rewrite");
            let updated = discovery_inputs_fingerprint(&root, &inputs).expect("updated");
            assert_ne!(initial, updated);
        });
    }

    #[test]
    fn discovery_input_rejects_parent_escape() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let err =
            discovery_inputs_fingerprint(&root, &["../escape".to_owned()]).expect_err("escape");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn new_index_uses_compact_json() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            let _ = nix_tree_fingerprint(&root).expect("fingerprint");

            let index_root = temp.path().join("fingerprint-index");
            let index_file = fs::read_dir(&index_root)
                .expect("read index dir")
                .map(|entry| entry.expect("entry").path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("index json file");
            let bytes = fs::read(&index_file).expect("read index");
            let text = String::from_utf8(bytes).expect("utf8 index");
            assert!(
                !text.contains("\n  "),
                "new indexes must use compact JSON, not pretty-printed"
            );
            assert!(text.contains("\"schema_version\":2"));
        });
    }

    #[test]
    fn legacy_pretty_index_loads_and_migrates() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            let expected = nix_tree_fingerprint(&root).expect("baseline");

            let index_root = temp.path().join("fingerprint-index");
            let index_file = fs::read_dir(&index_root)
                .expect("read index dir")
                .map(|entry| entry.expect("entry").path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("index json file");
            let mut index: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&index_file).expect("read index"))
                    .expect("parse index");
            index["schema_version"] = serde_json::json!(1);
            if let Some(entries) = index
                .get_mut("entries")
                .and_then(|value| value.as_object_mut())
            {
                for entry in entries.values_mut() {
                    if let Some(object) = entry.as_object_mut() {
                        object.remove("changed_ns");
                    }
                }
            }
            let pretty = serde_json::to_string_pretty(&index).expect("pretty legacy index");
            fs::write(&index_file, pretty).expect("write legacy pretty index");

            reset_file_read_counter();
            let loaded = nix_tree_fingerprint(&root).expect("load legacy");
            assert_eq!(loaded, expected);
            assert_eq!(
                reset_file_read_counter(),
                0,
                "legacy pretty index should reuse cached entry metadata"
            );

            let migrated = fs::read_to_string(&index_file).expect("read migrated");
            assert!(
                !migrated.contains("\n  "),
                "migration rewrite should store compact JSON"
            );
            assert!(migrated.contains("\"schema_version\":2"));
        });
    }

    #[test]
    fn force_rehash_env_rereads_unchanged_files() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("pkg.nix"), "pkg\n").expect("pkg");

            let _ = nix_tree_fingerprint(&root).expect("cold");
            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(reset_file_read_counter(), 0, "warm path skips file reads");

            let index_root = temp.path().join("fingerprint-index");
            let index_file = fs::read_dir(&index_root)
                .expect("read index dir")
                .map(|entry| entry.expect("entry").path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("index json file");
            let mut index: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&index_file).expect("read index"))
                    .expect("parse index");
            index["last_rehash_ns"] = serde_json::json!(0);
            fs::write(
                &index_file,
                serde_json::to_vec(&index).expect("serialize stale rehash timestamp"),
            )
            .expect("write stale index");

            super::set_test_force_rehash_secs(Some(1));
            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("forced rehash");
            assert_eq!(
                reset_file_read_counter(),
                2,
                "forced rehash should re-read every indexed file"
            );
            super::set_test_force_rehash_secs(None);
        });
    }

    #[cfg(unix)]
    #[test]
    fn ctime_mismatch_triggers_rehash() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            let _ = nix_tree_fingerprint(&root).expect("initial");

            let index_root = temp.path().join("fingerprint-index");
            let index_file = fs::read_dir(&index_root)
                .expect("read index dir")
                .map(|entry| entry.expect("entry").path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("index json file");
            let mut index: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&index_file).expect("read index"))
                    .expect("parse index");
            index["entries"]["flake.nix"]["changed_ns"] = serde_json::json!(1);
            fs::write(
                &index_file,
                serde_json::to_vec(&index).expect("serialize stale index"),
            )
            .expect("write stale index");

            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("after ctime mismatch");
            assert_eq!(
                reset_file_read_counter(),
                1,
                "ctime mismatch should re-read the file even when mtime matches"
            );
        });
    }

    #[test]
    fn flag_parsers_accept_the_documented_spellings() {
        assert!(parse_truthy("1"));
        assert!(parse_truthy("TRUE"));
        assert!(!parse_truthy("0"));
        assert!(!parse_truthy(""));
        assert!(!parse_enabled_flag("off", true));
        assert!(parse_enabled_flag("", true));
        assert!(!parse_enabled_flag("", false));
    }

    #[test]
    fn worktree_reference_requires_a_path_boundary() {
        assert!(flake_references_worktree(
            "inputs.x.url = ./wt-demo;",
            "wt-demo"
        ));
        assert!(flake_references_worktree(
            "inputs.x.url = path:./wt-demo;",
            "wt-demo"
        ));
        assert!(!flake_references_worktree(
            "inputs.x.url = ./wt-demo-extra;",
            "wt-demo"
        ));
        assert!(!flake_references_worktree(
            "description = \"wt-demo\";",
            "wt-demo"
        ));
    }

    #[test]
    fn independent_worktree_is_omitted_until_flake_nix_references_it() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::create_dir_all(root.join("wt-demo")).expect("mkdir");
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("flake");
            fs::write(root.join("wt-demo/a.nix"), "1\n").expect("worktree nix");

            reset_file_read_counter();
            let skipped = nix_tree_fingerprint(&root).expect("skip worktree");
            assert_eq!(reset_file_read_counter(), 1);

            let included =
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full walk");
            assert_ne!(skipped, included);

            fs::write(root.join("flake.nix"), "{ inputs.x.url = ./wt-demo; }\n").expect("refer");
            reset_file_read_counter();
            let referenced = nix_tree_fingerprint(&root).expect("referenced worktree");
            assert_eq!(reset_file_read_counter(), 2);
            assert_ne!(skipped, referenced);

            fs::write(
                root.join("flake.nix"),
                "{ script = ''\n  exec ./wt-demo\n''; }\n",
            )
            .expect("script mention");
            let scripted = nix_tree_fingerprint(&root).expect("script mention");
            fs::write(root.join("wt-demo/a.nix"), "changed\n").expect("edit worktree");
            let scripted_after = nix_tree_fingerprint(&root).expect("script mention after edit");
            assert_eq!(scripted, scripted_after);
        });
    }

    #[test]
    fn git_index_matches_walk_for_untracked_add_delete_and_rename() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::create_dir_all(root.join("nix")).expect("mkdir");
            fs::write(root.join("nix/a.nix"), "a\n").expect("tracked");
            run_git(&root, &["add", "flake.nix", "nix/a.nix"]);
            run_git(&root, &["commit", "-m", "init"]);

            let committed = nix_tree_fingerprint(&root).expect("committed");
            assert_scans_match(&root);

            fs::write(root.join("nix/extra.nix"), "extra\n").expect("untracked");
            let untracked = nix_tree_fingerprint(&root).expect("untracked");
            assert_ne!(committed, untracked);
            assert_scans_match(&root);

            run_git(&root, &["add", "nix/extra.nix"]);
            assert_scans_match(&root);

            fs::write(root.join("nix/extra.nix"), "edited\n").expect("unstaged edit");
            let unstaged = nix_tree_fingerprint(&root).expect("unstaged");
            assert_ne!(untracked, unstaged);
            assert_scans_match(&root);

            fs::remove_file(root.join("nix/a.nix")).expect("delete");
            let deleted = nix_tree_fingerprint(&root).expect("deleted");
            assert_ne!(unstaged, deleted);
            assert_scans_match(&root);

            run_git(&root, &["mv", "nix/extra.nix", "nix/renamed.nix"]);
            let renamed = nix_tree_fingerprint(&root).expect("renamed");
            assert_ne!(deleted, renamed);
            assert_scans_match(&root);
            fs::write(root.join("nix/renamed.nix"), "renamed\n").expect("edit");
            let edited = nix_tree_fingerprint(&root).expect("edited");
            assert_ne!(renamed, edited);
            let walked = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                .expect("walk");
            assert_eq!(edited, walked);
        });
    }

    #[test]
    fn git_index_skips_symlink_directories_and_ignored_nix_until_referenced() {
        let temp = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(outside.path().join("hidden.nix"), "1\n").expect("hidden");
            #[cfg(unix)]
            std::os::unix::fs::symlink(outside.path(), root.join("link")).expect("symlink");
            run_git(&root, &["add", "flake.nix"]);
            run_git(&root, &["commit", "-m", "init"]);

            reset_file_read_counter();
            let fingerprint = nix_tree_fingerprint(&root).expect("fingerprint");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            #[cfg(unix)]
            {
                assert_eq!(reset_file_read_counter(), 1, "symlink target is not hashed");
                let walked =
                    nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                        .expect("walk");
                assert_eq!(fingerprint, walked);
            }
            #[cfg(not(unix))]
            let _ = fingerprint;

            fs::create_dir_all(root.join(".venv")).expect("venv");
            fs::create_dir_all(root.join("dist")).expect("dist");
            fs::create_dir_all(root.join(".claude/worktrees")).expect("claude");
            fs::write(root.join(".venv/a.nix"), "venv\n").expect("venv nix");
            fs::write(root.join("dist/b.nix"), "dist\n").expect("dist nix");
            fs::write(root.join(".claude/worktrees/flake.nix"), "claude\n").expect("claude nix");
            fs::write(
                root.join(".gitignore"),
                ".venv/\ndist/\n.claude/\nsecret/\n",
            )
            .expect("gitignore");
            run_git(&root, &["add", ".gitignore"]);
            run_git(&root, &["commit", "-m", "ignore"]);
            let ignored = nix_tree_fingerprint(&root).expect("ignored dir");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::write(root.join(".venv/a.nix"), "changed\n").expect("edit ignored");
            assert_eq!(ignored, nix_tree_fingerprint(&root).expect("still ignored"));
            let ignored_walk =
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk ignored");
            assert_ne!(ignored, ignored_walk);
            let report = explain_nix_tree_fingerprint(&root).expect("explain");
            assert!(report.fast_path_accepted);
            assert!(
                report
                    .additional_filesystem_nix
                    .iter()
                    .any(|path| path == ".venv/a.nix")
            );

            fs::write(root.join("flake.nix"), "{ src = ./.venv; }\n").expect("reference");
            let referenced = nix_tree_fingerprint(&root).expect("referenced ignored dir");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_ne!(ignored, referenced);
            fs::write(root.join(".venv/a.nix"), "referenced\n").expect("edit referenced");
            assert_ne!(
                referenced,
                nix_tree_fingerprint(&root).expect("referenced edit")
            );
        });
    }

    #[test]
    fn git_visible_sources_include_a_gitlink_only_when_referenced() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let nested = root.join("nested");
        with_fingerprint_index_dir(&temp, || {
            fs::create_dir_all(&nested).expect("mkdir");
            init_repo(&nested);
            fs::write(nested.join("flake.nix"), "{ nested = true; }\n").expect("nested flake");
            run_git(&nested, &["add", "flake.nix"]);
            run_git(&nested, &["commit", "-m", "nested"]);

            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            run_git(&root, &["add", "flake.nix", "nested"]);
            run_git(&root, &["commit", "-m", "parent"]);
            let unreferenced = nix_tree_fingerprint(&root).expect("unreferenced gitlink");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            let walked = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                .expect("walk");
            assert_ne!(unreferenced, walked);

            fs::write(root.join("flake.nix"), "{ src = ./nested; }\n").expect("reference");
            let referenced = nix_tree_fingerprint(&root).expect("referenced gitlink");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_ne!(unreferenced, referenced);
            fs::write(nested.join("flake.nix"), "{ nested = false; }\n").expect("edit nested");
            let edited = nix_tree_fingerprint(&root).expect("edited gitlink");
            assert_ne!(referenced, edited);

            fs::remove_dir_all(nested.join(".git")).expect("uninitialize");
            let uninitialized = nix_tree_fingerprint(&root).expect("referenced uninitialized");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_eq!(uninitialized, edited);

            fs::write(root.join("flake.nix"), "{}\n").expect("drop reference");
            let dropped = nix_tree_fingerprint(&root).expect("unreferenced again");
            fs::write(nested.join("flake.nix"), "{ nested = 1; }\n").expect("edit dropped");
            assert_eq!(dropped, nix_tree_fingerprint(&root).expect("ignored edit"));
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
        });
    }

    #[test]
    fn missing_or_failing_git_falls_back_to_the_walk() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("app.nix"), "1\n").expect("nix");
            let walked = nix_tree_fingerprint(&root).expect("no git");
            assert_eq!(recorded_nix_scan(), SCAN_WALK);
            assert_eq!(
                walked,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk")
            );

            fs::write(root.join(".git"), "gitdir: /missing\n").expect("broken gitdir");
            let failed = nix_tree_fingerprint(&root).expect("broken git");
            assert_eq!(recorded_nix_scan(), SCAN_WALK);
            assert_eq!(failed, walked);
        });
    }

    #[test]
    fn renaming_a_referenced_directory_invalidates_discovery() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{ src = ./mods; }\n").expect("flake");
            fs::write(root.join(".gitignore"), "mods/\nhidden/\n").expect("gitignore");
            fs::create_dir_all(root.join("mods")).expect("mods");
            fs::write(root.join("mods/a.nix"), "a\n").expect("nix");
            run_git(&root, &["add", "flake.nix", ".gitignore"]);
            run_git(&root, &["commit", "-m", "init"]);
            let before = nix_tree_fingerprint(&root).expect("before");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::rename(root.join("mods"), root.join("hidden")).expect("rename");
            let after = nix_tree_fingerprint(&root).expect("after");
            assert_ne!(before, after);
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
        });
    }

    #[test]
    fn flake_source_literals_skip_strings_comments_and_reject_computed_paths() {
        let static_paths = analyze_flake_source(
            r#"
            { src = ./infra/app.nix; }
            script = ''
              exec ./lattice
              ''${not_a_path}
            '';
            # ./cell
            note = "delegates to ./devcell";
            stargateSrc = builtins.path { path = ./ProjectStargate; };
            other = path:./mods;
            "#,
        );
        assert!(static_paths.fallback.is_none());
        assert_eq!(
            static_paths.references,
            vec![
                "ProjectStargate".to_owned(),
                "infra/app.nix".to_owned(),
                "mods".to_owned(),
            ]
        );

        assert_eq!(
            analyze_flake_source(r#"import "${x}""#).fallback,
            Some("unresolvable import")
        );
        assert_eq!(
            analyze_flake_source("builtins.readDir other").fallback,
            Some("builtins.readDir")
        );
        assert_eq!(
            analyze_flake_source("src = ../outside;").fallback,
            Some("path escapes the flake root")
        );
        let read_dir = analyze_flake_source("builtins.readDir ./pkgs");
        assert!(read_dir.fallback.is_none());
        assert_eq!(read_dir.references, vec!["pkgs".to_owned()]);
    }

    #[test]
    fn non_utf8_git_path_is_not_classifiable() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let filter = WalkFilter {
            extra_ignore: &GlobSet::empty(),
            full_walk: false,
            references: &[],
        };
        let stage = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\t\xff.nix";
        let mut tracked = Vec::new();
        let mut paths = Vec::new();
        let classifiable =
            append_stage_nix(&root, "", stage, &filter, &mut tracked, &mut paths).expect("stage");
        assert!(!classifiable);
        assert!(paths.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn linked_worktree_nix_file_does_not_change_the_main_checkout() {
        let main = TempDir::new().expect("main");
        let linked_parent = TempDir::new().expect("linked parent");
        let root = utf8_root(&main);
        let link = camino::Utf8PathBuf::from_path_buf(linked_parent.path().join("tree"))
            .expect("utf8 worktree");
        with_fingerprint_index_dir(&main, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            run_git(&root, &["add", "flake.nix"]);
            run_git(&root, &["commit", "-m", "init"]);
            let before = nix_tree_fingerprint(&root).expect("before");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);

            run_git(&root, &["worktree", "add", "-b", "feature", link.as_str()]);
            fs::write(link.join("only.nix"), "only\n").expect("worktree file");
            let main_after = nix_tree_fingerprint(&root).expect("main after");
            assert_eq!(before, main_after);

            set_test_fingerprint_index_root(None);
            let link_index = linked_parent.path().join("fingerprint-index");
            fs::create_dir_all(&link_index).expect("link index");
            set_test_fingerprint_index_root(Some(link_index));
            let linked_fingerprint = nix_tree_fingerprint(&link).expect("linked");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            let linked_walk =
                nix_tree_fingerprint_scanning(&link, &GlobSet::empty(), ScanMode::Walk)
                    .expect("linked walk");
            assert_eq!(linked_fingerprint, linked_walk);
            assert_ne!(main_after, linked_fingerprint);
        });
    }

    #[test]
    fn discovery_input_outside_the_nix_set_still_changes_its_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("notes.txt"), "notes\n").expect("notes");
            run_git(&root, &["add", "flake.nix"]);
            run_git(&root, &["commit", "-m", "init"]);

            reset_file_read_counter();
            let _ = nix_tree_fingerprint(&root).expect("nix tree");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_eq!(reset_file_read_counter(), 1);

            let without = discovery_inputs_fingerprint(&root, &[]).expect("without");
            let with_notes =
                discovery_inputs_fingerprint(&root, &["notes.txt".to_owned()]).expect("with");
            assert_ne!(without, with_notes);
            let outside = discovery_inputs_fingerprint(&root, &["../outside".to_owned()]);
            assert!(outside.is_err(), "parent traversal is rejected");
        });
    }

    #[test]
    fn synthetic_large_tree_uses_the_git_index_and_skips_warm_rereads() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("flake");
            fs::create_dir_all(root.join("bulk")).expect("bulk");
            fs::create_dir_all(root.join("pkgs")).expect("pkgs");
            for index in 0..2_000 {
                fs::write(
                    root.join(format!("bulk/f{index}.txt")),
                    format!("{index}\n"),
                )
                .expect("txt");
            }
            for index in 0..10 {
                fs::write(
                    root.join(format!("pkgs/p{index}.nix")),
                    format!("{index}\n"),
                )
                .expect("nix");
            }
            run_git(&root, &["add", "."]);
            run_git(&root, &["commit", "-m", "init"]);

            reset_file_read_counter();
            reset_index_store_counter();
            let cold = nix_tree_fingerprint(&root).expect("cold");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_eq!(reset_file_read_counter(), 11);
            assert_eq!(reset_index_store_counter(), 1);

            reset_file_read_counter();
            reset_index_store_counter();
            let warm = nix_tree_fingerprint(&root).expect("warm");
            assert_eq!(cold, warm);
            assert_eq!(reset_file_read_counter(), 0);
            assert_eq!(reset_index_store_counter(), 0);

            let walked = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                .expect("walk");
            assert_eq!(cold, walked);
        });
    }

    #[test]
    fn tracked_nix_content_changes_the_git_visible_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::create_dir_all(root.join("nix")).expect("mkdir");
            fs::write(root.join("nix/module.nix"), "module\n").expect("module");
            fs::write(root.join("README.md"), "readme\n").expect("readme");
            run_git(&root, &["add", "flake.nix", "nix/module.nix", "README.md"]);
            run_git(&root, &["commit", "-m", "init"]);

            let base = nix_tree_fingerprint(&root).expect("base");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_scans_match(&root);

            fs::write(root.join("README.md"), "readme changed\n").expect("edit readme");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("readme"),
                "non-nix files are outside the nix-tree fingerprint"
            );

            fs::write(root.join("nix/module.nix"), "module changed\n").expect("edit module");
            let edited = nix_tree_fingerprint(&root).expect("edited module");
            assert_ne!(base, edited, "tracked nix content is copied");
            assert_scans_match(&root);

            fs::write(root.join("flake.lock"), "{}\n").expect("lock");
            let locked = nix_tree_fingerprint(&root).expect("lock");
            assert_ne!(edited, locked, "flake.lock is part of the copied source");
            fs::write(root.join("flake.lock"), "{\n  \"x\": 1\n}\n").expect("edit lock");
            assert_ne!(
                locked,
                nix_tree_fingerprint(&root).expect("edited lock"),
                "flake.lock content changes the fingerprint"
            );
        });
    }

    #[test]
    fn staged_addition_and_deletion_track_the_copied_source() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            run_git(&root, &["add", "flake.nix"]);
            run_git(&root, &["commit", "-m", "init"]);
            let base = nix_tree_fingerprint(&root).expect("base");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);

            fs::create_dir_all(root.join("lib")).expect("mkdir");
            fs::write(root.join("lib/new.nix"), "added\n").expect("new");
            run_git(&root, &["add", "--", "lib/new.nix"]);
            let staged = nix_tree_fingerprint(&root).expect("staged addition");
            assert_ne!(base, staged, "a staged nix file is copied");
            assert_scans_match(&root);

            run_git(&root, &["rm", "-f", "--", "lib/new.nix"]);
            let deleted = nix_tree_fingerprint(&root).expect("staged deletion");
            assert_ne!(staged, deleted, "a staged deletion drops the file");
            assert_eq!(deleted, base);
            assert_scans_match(&root);

            fs::create_dir_all(root.join("lib")).expect("mkdir again");
            fs::write(root.join("lib/new.nix"), "added\n").expect("restore");
            run_git(&root, &["add", "--", "lib/new.nix"]);
            run_git(&root, &["commit", "-m", "add lib"]);
            let committed = nix_tree_fingerprint(&root).expect("committed");
            assert_eq!(committed, staged);

            run_git(&root, &["rm", "--cached", "--", "lib/new.nix"]);
            let still_present = nix_tree_fingerprint(&root).expect("cached removal");
            assert_eq!(
                still_present, committed,
                "a staged index deletion that leaves a non-ignored file is still copied"
            );
            assert_scans_match(&root);

            fs::write(root.join("lib/new.nix"), "ignored-edit\n").expect("edit before ignore");
            fs::write(root.join(".gitignore"), "lib/\n").expect("gitignore");
            run_git(&root, &["add", "--", ".gitignore"]);
            run_git(&root, &["commit", "-m", "ignore lib"]);
            let ignored = nix_tree_fingerprint(&root).expect("ignored");
            assert_eq!(ignored, base, "an ignored untracked nix file is not copied");
            fs::write(root.join("lib/new.nix"), "still-ignored\n").expect("edit ignored");
            assert_eq!(
                ignored,
                nix_tree_fingerprint(&root).expect("ignored edit"),
                "editing an ignored untracked file does not invalidate"
            );

            fs::write(root.join("lib/new.nix"), "added\n").expect("restore bytes");
            run_git(&root, &["add", "-f", "--", "lib/new.nix"]);
            let forced = nix_tree_fingerprint(&root).expect("force added");
            assert_eq!(forced, committed, "a force-added nix file is copied again");
            assert_scans_match(&root);
        });
    }

    #[test]
    fn untracked_non_ignored_nix_changes_the_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            run_git(&root, &["add", "flake.nix"]);
            run_git(&root, &["commit", "-m", "init"]);
            let base = nix_tree_fingerprint(&root).expect("base");

            fs::create_dir_all(root.join("fresh")).expect("mkdir");
            fs::write(root.join("fresh/a.nix"), "1\n").expect("untracked");
            let untracked = nix_tree_fingerprint(&root).expect("untracked dir");
            assert_ne!(base, untracked, "an untracked directory of nix is copied");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_scans_match(&root);

            fs::write(root.join("fresh/a.nix"), "22\n").expect("edit untracked");
            let edited = nix_tree_fingerprint(&root).expect("edited untracked");
            assert_ne!(untracked, edited);
            assert_scans_match(&root);

            fs::write(root.join("fresh/notes.txt"), "notes\n").expect("notes");
            assert_eq!(
                edited,
                nix_tree_fingerprint(&root).expect("notes"),
                "untracked non-nix files are outside the nix-tree fingerprint"
            );

            fs::write(root.join(".gitignore"), "fresh/\n").expect("gitignore");
            let ignored = nix_tree_fingerprint(&root).expect("ignored");
            assert_eq!(ignored, base, "gitignored untracked nix is not copied");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::write(root.join("fresh/a.nix"), "333\n").expect("edit ignored");
            assert_eq!(ignored, nix_tree_fingerprint(&root).expect("still ignored"));
        });
    }

    #[test]
    fn ignored_unreferenced_worktree_does_not_change_the_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("flake");
            fs::write(root.join(".gitignore"), ".claude/\n").expect("gitignore");
            run_git(&root, &["add", "flake.nix", ".gitignore"]);
            run_git(&root, &["commit", "-m", "init"]);
            let base = nix_tree_fingerprint(&root).expect("base");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);

            fs::create_dir_all(root.join("wt-scratch")).expect("wt");
            fs::write(root.join("wt-scratch/a.nix"), "1\n").expect("wt nix");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("unreferenced wt"),
                "an unreferenced wt-* tree is not copied"
            );
            assert_eq!(
                base,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk skips wt")
            );
            let full = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::FullWalk)
                .expect("full");
            assert_ne!(base, full);
            fs::write(root.join("wt-scratch/a.nix"), "22\n").expect("edit wt");
            assert_eq!(base, nix_tree_fingerprint(&root).expect("wt edit"));
            assert_ne!(
                full,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full sees wt edit")
            );

            fs::create_dir_all(root.join(".claude/worktrees/agent")).expect("claude");
            fs::write(root.join(".claude/worktrees/agent/flake.nix"), "claude\n")
                .expect("claude flake");
            fs::write(root.join(".claude/worktrees/agent/module.nix"), "module\n")
                .expect("claude module");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("ignored worktree"),
                "a gitignored worktree is not copied"
            );
            let walked = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                .expect("walk sees ignored worktree");
            assert_ne!(base, walked);
            fs::write(
                root.join(".claude/worktrees/agent/module.nix"),
                "module-2\n",
            )
            .expect("edit claude");
            assert_eq!(base, nix_tree_fingerprint(&root).expect("claude edit"));
            assert_ne!(
                walked,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk sees claude edit")
            );

            fs::write(root.join("flake.nix"), "{ src = ./wt-scratch; }\n").expect("refer wt");
            let referenced = nix_tree_fingerprint(&root).expect("referenced wt");
            fs::write(root.join("wt-scratch/a.nix"), "333\n").expect("edit referenced wt");
            let referenced_edit = nix_tree_fingerprint(&root).expect("referenced wt edit");
            assert_ne!(referenced, referenced_edit);
            fs::write(
                root.join(".claude/worktrees/agent/module.nix"),
                "module-3\n",
            )
            .expect("claude still ignored");
            assert_eq!(
                referenced_edit,
                nix_tree_fingerprint(&root).expect("claude still ignored")
            );

            fs::write(
                root.join("flake.nix"),
                "{ src = ./.claude/worktrees/agent; }\n",
            )
            .expect("refer claude");
            let claude_ref = nix_tree_fingerprint(&root).expect("referenced claude");
            fs::write(root.join("wt-scratch/a.nix"), "4444\n").expect("wt dropped");
            assert_eq!(
                claude_ref,
                nix_tree_fingerprint(&root).expect("wt no longer referenced"),
                "dropping the wt-* reference stops copying that tree"
            );
            fs::write(
                root.join(".claude/worktrees/agent/module.nix"),
                "module-4\n",
            )
            .expect("edit referenced claude");
            assert_ne!(
                claude_ref,
                nix_tree_fingerprint(&root).expect("referenced claude edit")
            );
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
        });
    }

    #[test]
    fn explicit_relative_import_hashes_only_the_named_source() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("flake");
            fs::write(root.join(".gitignore"), "vendor/\n").expect("gitignore");
            run_git(&root, &["add", "flake.nix", ".gitignore"]);
            run_git(&root, &["commit", "-m", "init"]);
            fs::create_dir_all(root.join("vendor")).expect("vendor");
            fs::write(root.join("vendor/lib.nix"), "lib\n").expect("lib");
            fs::write(root.join("vendor/other.nix"), "other\n").expect("other");

            let base = nix_tree_fingerprint(&root).expect("unreferenced");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::write(root.join("vendor/lib.nix"), "lib-2\n").expect("edit ignored");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("ignored import target"),
                "an ignored tree is not copied until flake.nix names it"
            );

            fs::write(
                root.join("flake.nix"),
                "{\n  # ./vendor/lib.nix\n  note = \"import ./vendor/lib.nix\";\n}\n",
            )
            .expect("mention");
            let mentioned = nix_tree_fingerprint(&root).expect("mention");
            fs::write(root.join("vendor/lib.nix"), "lib-3\n").expect("edit mention");
            assert_eq!(
                mentioned,
                nix_tree_fingerprint(&root).expect("string mention"),
                "comments and strings do not name a copied path"
            );

            for (label, flake) in [
                ("import", "{ import ./vendor/lib.nix; }\n"),
                ("path literal", "{ src = path:./vendor/lib.nix; }\n"),
                (
                    "builtins.path",
                    "{ src = builtins.path { path = ./vendor/lib.nix; }; }\n",
                ),
            ] {
                fs::write(root.join("flake.nix"), flake).expect(label);
                let named = nix_tree_fingerprint(&root).expect(label);
                assert_eq!(recorded_nix_scan(), SCAN_GIT);
                fs::write(root.join("vendor/other.nix"), format!("other-{label}\n"))
                    .expect("edit other");
                assert_eq!(
                    named,
                    nix_tree_fingerprint(&root).expect("unnamed sibling"),
                    "{label}: an unnamed ignored sibling is not copied"
                );
                fs::write(root.join("vendor/lib.nix"), format!("lib-{label}\n")).expect("edit lib");
                assert_ne!(
                    named,
                    nix_tree_fingerprint(&root).expect("named file"),
                    "{label}: the named relative import is copied"
                );
            }

            fs::write(root.join("flake.nix"), "{ src = ./vendor; }\n").expect("directory");
            let directory = nix_tree_fingerprint(&root).expect("directory reference");
            fs::write(root.join("vendor/other.nix"), "other-dir\n").expect("edit dir sibling");
            assert_ne!(
                directory,
                nix_tree_fingerprint(&root).expect("directory member"),
                "a named relative directory copies the nix files under it"
            );
        });
    }

    #[test]
    fn explicit_import_inside_a_gitlink_hashes_only_that_file() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let nested = root.join("nested");
        with_fingerprint_index_dir(&temp, || {
            fs::create_dir_all(&nested).expect("mkdir");
            init_repo(&nested);
            fs::write(nested.join("lib.nix"), "lib\n").expect("lib");
            fs::write(nested.join("other.nix"), "other\n").expect("other");
            run_git(&nested, &["add", "lib.nix", "other.nix"]);
            run_git(&nested, &["commit", "-m", "nested"]);

            init_repo(&root);
            fs::write(root.join("flake.nix"), "{ import ./nested/lib.nix; }\n").expect("flake");
            run_git(&root, &["add", "flake.nix", "nested"]);
            run_git(&root, &["commit", "-m", "parent"]);

            let referenced = nix_tree_fingerprint(&root).expect("referenced");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::write(nested.join("lib.nix"), "lib-2\n").expect("edit lib");
            let edited = nix_tree_fingerprint(&root).expect("edited lib");
            assert_ne!(
                referenced, edited,
                "the named file inside the gitlink is copied"
            );
            fs::write(nested.join("other.nix"), "other-2\n").expect("edit other");
            assert_eq!(
                edited,
                nix_tree_fingerprint(&root).expect("edited other"),
                "an unnamed file inside the gitlink is not copied"
            );

            fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("drop");
            let dropped = nix_tree_fingerprint(&root).expect("dropped");
            fs::write(nested.join("lib.nix"), "lib-3\n").expect("edit dropped");
            assert_eq!(
                dropped,
                nix_tree_fingerprint(&root).expect("unreferenced edit"),
                "an unreferenced gitlink is not copied"
            );
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
        });
    }

    #[cfg(unix)]
    #[test]
    fn symlink_nix_paths_are_not_part_of_the_copied_source() {
        let temp = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside");
        let root = utf8_root(&temp);
        let outside_root =
            camino::Utf8PathBuf::from_path_buf(outside.path().to_path_buf()).expect("utf8 outside");
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("tracked.nix"), "tracked\n").expect("tracked");
            std::os::unix::fs::symlink(root.join("tracked.nix"), root.join("alias.nix"))
                .expect("alias");
            std::os::unix::fs::symlink(outside.path(), root.join("link")).expect("linkdir");
            fs::write(outside_root.join("hidden.nix"), "hidden\n").expect("hidden");
            run_git(
                &root,
                &["add", "--", "flake.nix", "tracked.nix", "alias.nix", "link"],
            );
            run_git(&root, &["commit", "-m", "init"]);

            let base = nix_tree_fingerprint(&root).expect("base");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::remove_file(root.join("alias.nix")).expect("remove alias");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("without alias"),
                "a tracked symlink is not hashed"
            );
            fs::write(root.join("alias.nix"), "tracked\n").expect("regular alias");
            run_git(&root, &["add", "--", "alias.nix"]);
            assert_ne!(
                base,
                nix_tree_fingerprint(&root).expect("regular alias"),
                "replacing the symlink with a staged regular nix file changes the fingerprint"
            );
            run_git(&root, &["rm", "-f", "--", "alias.nix"]);
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("alias removed"),
                "deleting that regular file restores the copied source"
            );

            std::os::unix::fs::symlink(root.join("tracked.nix"), root.join("extra.nix"))
                .expect("untracked symlink");
            assert_eq!(
                base,
                nix_tree_fingerprint(&root).expect("untracked symlink"),
                "an untracked symlink is not hashed"
            );
            fs::remove_file(root.join("extra.nix")).expect("remove extra symlink");
            fs::write(root.join("extra.nix"), "extra\n").expect("untracked file");
            assert_ne!(
                base,
                nix_tree_fingerprint(&root).expect("untracked file"),
                "an untracked regular nix file is copied"
            );

            fs::write(root.join("flake.nix"), "{ src = ./link; }\n").expect("reference link");
            let referenced = nix_tree_fingerprint(&root).expect("referenced symlink");
            fs::write(outside_root.join("hidden.nix"), "changed\n").expect("edit target");
            assert_eq!(
                referenced,
                nix_tree_fingerprint(&root).expect("symlink target edited"),
                "a referenced symlink directory is not followed"
            );
            fs::write(root.join("tracked.nix"), "tracked-2\n").expect("edit tracked");
            assert_ne!(
                referenced,
                nix_tree_fingerprint(&root).expect("tracked edited"),
                "a regular tracked nix file still invalidates"
            );
        });
    }

    #[test]
    fn unusual_nix_filenames_change_the_git_visible_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            init_repo(&root);
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            let mut names = vec![
                "odd name.nix".to_owned(),
                "my nix/mod.nix".to_owned(),
                "--odd.nix".to_owned(),
                "λ.nix".to_owned(),
                "o'brien.nix".to_owned(),
                ".hidden.nix".to_owned(),
                "star*.nix".to_owned(),
            ];
            #[cfg(unix)]
            {
                names.push("a\tb.nix".to_owned());
                names.push("line\nbreak.nix".to_owned());
            }
            for name in &names {
                let path = root.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).expect("mkdir");
                }
                fs::write(&path, "1\n").expect("write unusual name");
            }
            run_git(&root, &["add", "-A"]);
            run_git(&root, &["commit", "-m", "init"]);

            let mut previous = nix_tree_fingerprint(&root).expect("base");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            assert_scans_match(&root);
            for (index, name) in names.iter().enumerate() {
                fs::write(root.join(name), format!("changed-{index}\n")).expect("edit unusual");
                let updated = nix_tree_fingerprint(&root).expect("edited unusual");
                assert_ne!(
                    previous, updated,
                    "git-visible nix file {name:?} is part of the copied source"
                );
                assert_eq!(recorded_nix_scan(), SCAN_GIT);
                assert_scans_match(&root);
                previous = updated;
            }

            fs::write(root.join("notes.NIX"), "not nix\n").expect("wrong extension");
            assert_eq!(
                previous,
                nix_tree_fingerprint(&root).expect("capital extension"),
                "only a lowercase .nix extension is hashed"
            );

            fs::write(root.join(".gitignore"), "secret dir/\n").expect("gitignore");
            run_git(&root, &["add", "--", ".gitignore"]);
            run_git(&root, &["commit", "-m", "ignore secret"]);
            fs::create_dir_all(root.join("secret dir")).expect("secret dir");
            fs::write(root.join("secret dir/odd name.nix"), "secret\n").expect("secret nix");
            let ignored = nix_tree_fingerprint(&root).expect("ignored unusual");
            fs::write(root.join("secret dir/odd name.nix"), "secret-2\n").expect("edit secret");
            assert_eq!(
                ignored,
                nix_tree_fingerprint(&root).expect("ignored unusual edit"),
                "an ignored unusual filename is not copied"
            );
        });
    }

    #[test]
    fn compatibility_walk_tracks_nix_source_without_the_git_index() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        with_fingerprint_index_dir(&temp, || {
            assert_eq!(
                production_scan_mode(),
                ScanMode::Auto,
                "git index enumeration stays enabled unless the env switches say otherwise"
            );
            fs::write(root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(root.join("app.nix"), "1\n").expect("app");
            fs::write(root.join(".gitignore"), "hidden/\n").expect("gitignore");
            fs::create_dir_all(root.join("hidden")).expect("hidden");
            fs::write(root.join("hidden/a.nix"), "hidden\n").expect("hidden nix");
            fs::create_dir_all(root.join("wt-local")).expect("wt");
            fs::write(root.join("wt-local/a.nix"), "wt\n").expect("wt nix");

            let missing_git = nix_tree_fingerprint(&root).expect("missing git");
            assert_eq!(recorded_nix_scan(), SCAN_WALK);
            assert_eq!(
                missing_git,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk")
            );
            fs::write(root.join("app.nix"), "22\n").expect("edit app");
            let app_edited = nix_tree_fingerprint(&root).expect("edited app");
            assert_ne!(missing_git, app_edited);
            fs::write(root.join("hidden/a.nix"), "hidden-2\n").expect("edit hidden");
            let hidden_edited = nix_tree_fingerprint(&root).expect("edited hidden");
            assert_ne!(
                app_edited, hidden_edited,
                "without git, a gitignore does not hide nix files from the walk"
            );
            fs::write(root.join("wt-local/a.nix"), "wt-2\n").expect("edit wt");
            assert_eq!(
                hidden_edited,
                nix_tree_fingerprint(&root).expect("wt skipped"),
                "a missing git command still skips an unreferenced wt-* tree"
            );
            let full = nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::FullWalk)
                .expect("full");
            assert_ne!(hidden_edited, full);
            fs::write(root.join("wt-local/a.nix"), "wt-3\n").expect("edit wt again");
            assert_ne!(
                full,
                nix_tree_fingerprint_scanning(&root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full edit"),
                "NXR_FINGERPRINT_FULL_WALK=1 includes an independent wt-* tree"
            );
            assert_eq!(
                hidden_edited,
                nix_tree_fingerprint(&root).expect("production still skips wt")
            );
        });

        let git_temp = TempDir::new().expect("git tempdir");
        let git_root = utf8_root(&git_temp);
        with_fingerprint_index_dir(&git_temp, || {
            init_repo(&git_root);
            fs::write(git_root.join("flake.nix"), "{}\n").expect("flake");
            fs::write(git_root.join(".gitignore"), "hidden/\n").expect("gitignore");
            run_git(&git_root, &["add", "flake.nix", ".gitignore"]);
            run_git(&git_root, &["commit", "-m", "init"]);
            fs::create_dir_all(git_root.join("hidden")).expect("hidden");
            fs::write(git_root.join("hidden/a.nix"), "1\n").expect("hidden nix");
            fs::create_dir_all(git_root.join("wt-local")).expect("wt");
            fs::write(git_root.join("wt-local/a.nix"), "1\n").expect("wt nix");

            let git_fp = nix_tree_fingerprint(&git_root).expect("git");
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
            fs::write(git_root.join("hidden/a.nix"), "22\n").expect("edit hidden");
            assert_eq!(
                git_fp,
                nix_tree_fingerprint(&git_root).expect("ignored edit"),
                "an ignored unreferenced tree does not change the git fingerprint"
            );
            fs::write(git_root.join("wt-local/a.nix"), "22\n").expect("edit wt");
            assert_eq!(git_fp, nix_tree_fingerprint(&git_root).expect("wt edit"));

            let walk_before =
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk before");
            assert_ne!(git_fp, walk_before);
            fs::write(git_root.join("hidden/a.nix"), "333\n").expect("edit hidden for walk");
            let walk_after =
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk after");
            assert_ne!(
                walk_before, walk_after,
                "NXR_NIX_TREE_GIT_INDEX=off walks gitignored nix files"
            );
            fs::write(git_root.join("wt-local/a.nix"), "333\n").expect("edit wt for walk");
            assert_eq!(
                walk_after,
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::Walk)
                    .expect("walk still skips wt"),
                "NXR_NIX_TREE_GIT_INDEX=off still skips an unreferenced wt-* tree"
            );

            let full_before =
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full before");
            fs::write(git_root.join("wt-local/a.nix"), "4444\n").expect("edit wt for full");
            let full_after =
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full after");
            assert_ne!(
                full_before, full_after,
                "NXR_FINGERPRINT_FULL_WALK=1 hashes an independent wt-* tree"
            );
            fs::write(git_root.join("hidden/a.nix"), "55555\n").expect("edit hidden for full");
            assert_ne!(
                full_after,
                nix_tree_fingerprint_scanning(&git_root, &GlobSet::empty(), ScanMode::FullWalk)
                    .expect("full sees ignored"),
                "NXR_FINGERPRINT_FULL_WALK=1 also hashes gitignored nix files"
            );

            fs::write(git_root.join("flake.nix"), "{ changed = true; }\n").expect("edit flake");
            assert_ne!(
                git_fp,
                nix_tree_fingerprint(&git_root).expect("flake edit"),
                "a nix file Nix would copy still changes the git fingerprint"
            );
            assert_eq!(recorded_nix_scan(), SCAN_GIT);
        });
    }

    fn assert_scans_match(root: &camino::Utf8Path) {
        let git = nix_tree_fingerprint(root).expect("git fingerprint");
        assert_eq!(recorded_nix_scan(), SCAN_GIT);
        let walked =
            nix_tree_fingerprint_scanning(root, &GlobSet::empty(), ScanMode::Walk).expect("walk");
        assert_eq!(git, walked);
    }

    fn init_repo(root: &camino::Utf8Path) {
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "test"]);
    }

    fn run_git(root: &camino::Utf8Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-C",
                root.as_str(),
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
}
