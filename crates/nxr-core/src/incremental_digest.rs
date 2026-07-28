//! Incremental workspace path digests for action keys ([ADR-0155]).
//!
//! Extends run-scoped memo ([ADR-0154]) with:
//! - metadata-gated reuse of prior content digests (durable index)
//! - Git blob identity for clean tracked files (batched index/status)
//!
//! `cas::digest_repo_path` remains pure content hashing for CAS verify/save.
//! Store paths stay out of workspace digests ([ADR-0147]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use blake3::Hasher;
use camino::Utf8Path;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::sync::Mutex;

use crate::cas::{collect_files, digest_file, ensure_within_workspace, missing_path_digest};
use crate::merkle_index::{MerkleSession, digest_dir_merkle, merkle_index_enabled};
use crate::perf::{
    add_bytes_hashed, record_digest_metadata_hit, record_fs_metadata, record_git_blob_digest,
};

/// Kill-switch for Git blob digests (`off` / `0` / `false` / `no`).
pub const GIT_DIGESTS_ENV: &str = "NXR_GIT_DIGESTS";

/// Kill-switch for the durable action-digest index (`off` / `0` / `false` / `no`).
pub const ACTION_DIGEST_INDEX_ENV: &str = "NXR_ACTION_DIGEST_INDEX";

/// On-disk index schema version.
pub const ACTION_DIGEST_INDEX_SCHEMA_VERSION: u32 = 1;

/// Domain tag for mapping a Git blob OID into action-key digest material.
///
/// Clean tracked files contribute
/// `BLAKE3("nxr.action-digest.git-blob.v1" ‖ 0x00 ‖ oid_hex_ascii)` rather than
/// hashing working-tree bytes. Identical trees (same blob OIDs) therefore yield
/// identical digests; content-equivalent untracked or dirty files may differ.
pub const GIT_BLOB_DIGEST_DOMAIN: &[u8] = b"nxr.action-digest.git-blob.v1";

#[cfg(test)]
static TEST_INDEX_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Test override: 0 = follow env, 1 = force off, 2 = force on.
#[cfg(test)]
static TEST_GIT_DIGESTS: Mutex<u8> = Mutex::new(0);
#[cfg(test)]
static TEST_ACTION_DIGEST_INDEX: Mutex<u8> = Mutex::new(0);

#[cfg(test)]
const TEST_FOLLOW_ENV: u8 = 0;
#[cfg(test)]
const TEST_FORCE_OFF: u8 = 1;
#[cfg(test)]
const TEST_FORCE_ON: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ActionDigestEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_identity: Option<FileIdentity>,
    size: u64,
    modified_ns: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_ns: Option<u128>,
    content_hash: String,
    /// Present when `content_hash` was derived from this Git blob OID (hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    git_blob: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ActionDigestIndex {
    schema_version: u32,
    root: String,
    entries: BTreeMap<String, ActionDigestEntry>,
}

impl ActionDigestEntry {
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
}

/// Batched Git index + dirty set for one flake root (one status + one ls-files).
#[derive(Clone, Debug, Default)]
pub struct GitDigestSnapshot {
    /// stage-0 blob OID (hex) by repo-relative path.
    blobs: HashMap<String, String>,
    /// Paths that are dirty, staged, untracked, or conflicted in status.
    dirty_or_untracked: HashSet<String>,
}

impl GitDigestSnapshot {
    fn blob_for_clean(&self, relative: &str) -> Option<&str> {
        if self.dirty_or_untracked.contains(relative) {
            return None;
        }
        self.blobs.get(relative).map(String::as_str)
    }
}

/// Mutable state shared across digests within one [`RunDigestCache`](crate::RunDigestCache).
#[derive(Debug, Default)]
pub struct IncrementalDigestState {
    git: Option<Option<GitDigestSnapshot>>,
    index: Option<IndexSession>,
    merkle: Option<MerkleSession>,
    /// Paths digested this session that updated the durable index.
    dirty: bool,
}

#[derive(Debug)]
struct IndexSession {
    path: PathBuf,
    index: ActionDigestIndex,
    loaded: ActionDigestIndex,
}

impl IncrementalDigestState {
    /// Empty state for one invocation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist durable index updates if any.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the index cannot be written.
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(session) = self.index.as_mut()
            && self.dirty
            && session.index != session.loaded
        {
            store_action_digest_index(&session.path, &session.index)?;
            session.loaded = session.index.clone();
            self.dirty = false;
        }
        if let Some(merkle) = self.merkle.as_mut() {
            merkle.flush()?;
        }
        Ok(())
    }

    /// Borrow the Merkle session when present (Wave 4 / watch hooks).
    #[must_use]
    pub fn merkle_session(&self) -> Option<&MerkleSession> {
        self.merkle.as_ref()
    }

    /// Mutable Merkle session when present.
    pub fn merkle_session_mut(&mut self) -> Option<&mut MerkleSession> {
        self.merkle.as_mut()
    }

    pub(crate) fn set_merkle_session(&mut self, session: MerkleSession) {
        self.merkle = Some(session);
    }

    /// Drop batched Git status so the next digest reloads dirty/clean sets.
    pub fn reset_git_snapshot(&mut self) {
        self.git = None;
    }
}

impl Drop for IncrementalDigestState {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Drop durable action-digest entries for changed paths (watch / daemon hook).
///
/// # Errors
///
/// Returns [`io::Error`] when the index cannot be read.
pub fn invalidate_action_digest_paths(
    flake_root: &Utf8Path,
    paths: &[String],
    state: &mut IncrementalDigestState,
) -> io::Result<usize> {
    if paths.is_empty() || !action_digest_index_enabled() {
        return Ok(0);
    }
    let Some(session) = ensure_index_session(flake_root, state)? else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for path in paths {
        let normalized = path.replace('\\', "/");
        if session.index.entries.remove(&normalized).is_some() {
            removed += 1;
        }
    }
    if removed > 0 {
        state.dirty = true;
    }
    Ok(removed)
}

/// Whether Git blob digests are enabled (default on; kill-switch via env).
#[must_use]
pub fn git_digests_enabled() -> bool {
    #[cfg(test)]
    if let Ok(guard) = TEST_GIT_DIGESTS.lock() {
        match *guard {
            TEST_FORCE_OFF => return false,
            TEST_FORCE_ON => return true,
            _ => {}
        }
    }
    env_flag_enabled(GIT_DIGESTS_ENV, true)
}

/// Whether the durable action-digest index is enabled (default on).
#[must_use]
pub fn action_digest_index_enabled() -> bool {
    #[cfg(test)]
    if let Ok(guard) = TEST_ACTION_DIGEST_INDEX.lock() {
        match *guard {
            TEST_FORCE_OFF => return false,
            TEST_FORCE_ON => return true,
            _ => {}
        }
    }
    env_flag_enabled(ACTION_DIGEST_INDEX_ENV, true)
}

fn env_flag_enabled(name: &str, default_on: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let normalized = value.trim();
            if normalized.is_empty() {
                return default_on;
            }
            !matches!(
                normalized.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        }
        Err(_) => default_on,
    }
}

/// Map a Git blob OID (hex ASCII) into action-key digest material.
#[must_use]
pub fn digest_from_git_blob(oid_hex: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(GIT_BLOB_DIGEST_DOMAIN);
    hasher.update(&[0]);
    hasher.update(oid_hex.as_bytes());
    let digest = hasher.finalize();
    // Domain string only — not working-tree bytes.
    add_bytes_hashed((GIT_BLOB_DIGEST_DOMAIN.len() + 1 + oid_hex.len()) as u64);
    record_git_blob_digest();
    digest.to_hex().to_string()
}

/// Digest a repo-relative path using metadata gating and optional Git blob identity.
///
/// # Errors
///
/// Returns [`io::Error`] when traversal, Git batching, or reading fails.
pub fn digest_repo_path_incremental(
    flake_root: &Utf8Path,
    relative: &str,
    state: &mut IncrementalDigestState,
) -> io::Result<String> {
    let root = flake_root.as_std_path();
    let path = root.join(relative);
    if !path.exists() {
        return Ok(missing_path_digest());
    }
    ensure_within_workspace(root, &path)?;
    if path.is_file() {
        return digest_file_incremental(flake_root, relative, &path, state);
    }
    if merkle_index_enabled() {
        return digest_dir_merkle(flake_root, relative, state);
    }
    // Flat walk (pre-Merkle / kill-switch): hash every descendant file.
    let mut hasher = Hasher::new();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, &path, &mut files)?;
    files.sort();
    for file in files {
        let dir_rel = file.strip_prefix(&path).unwrap_or(&file).to_string_lossy();
        let repo_rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(dir_rel.as_bytes());
        hasher.update(digest_file_incremental(flake_root, &repo_rel, &file, state)?.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Leaf digest for Merkle directory aggregation ([ADR-0156]).
///
/// # Errors
///
/// Returns [`io::Error`] when reading or Git batching fails.
pub fn digest_file_for_merkle(
    flake_root: &Utf8Path,
    relative: &str,
    absolute: &Path,
    state: &mut IncrementalDigestState,
) -> io::Result<String> {
    digest_file_incremental(flake_root, relative, absolute, state)
}

fn digest_file_incremental(
    flake_root: &Utf8Path,
    relative: &str,
    absolute: &Path,
    state: &mut IncrementalDigestState,
) -> io::Result<String> {
    let normalized = relative.replace('\\', "/");

    if git_digests_enabled() {
        let oid = ensure_git_snapshot(flake_root, state)?
            .and_then(|snapshot| snapshot.blob_for_clean(&normalized).map(str::to_owned));
        if let Some(oid) = oid {
            let digest = digest_from_git_blob(&oid);
            maybe_store_git_entry(flake_root, &normalized, absolute, &oid, &digest, state)?;
            return Ok(digest);
        }
    }

    let metadata = fs::metadata(absolute)?;
    record_fs_metadata();
    let file_identity = file_identity(&metadata);
    let size = metadata.len();
    let modified_ns = modified_ns(&metadata)?;
    let changed_ns = changed_ns(&metadata);

    if let Some(prior) = prior_content_entry(flake_root, &normalized, state)?
        && prior.git_blob.is_none()
        && prior.matches_metadata(file_identity, size, modified_ns, changed_ns)
    {
        record_digest_metadata_hit();
        return Ok(prior.content_hash.clone());
    }

    let digest = digest_file(absolute)?;
    store_content_entry(
        flake_root,
        &normalized,
        file_identity,
        size,
        modified_ns,
        changed_ns,
        digest.clone(),
        state,
    )?;
    Ok(digest)
}

fn maybe_store_git_entry(
    flake_root: &Utf8Path,
    relative: &str,
    absolute: &Path,
    oid: &str,
    digest: &str,
    state: &mut IncrementalDigestState,
) -> io::Result<()> {
    if !action_digest_index_enabled() {
        return Ok(());
    }
    let metadata = match fs::metadata(absolute) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    record_fs_metadata();
    let entry = ActionDigestEntry {
        file_identity: file_identity(&metadata),
        size: metadata.len(),
        modified_ns: modified_ns(&metadata).unwrap_or(0),
        changed_ns: changed_ns(&metadata),
        content_hash: digest.to_owned(),
        git_blob: Some(oid.to_owned()),
    };
    upsert_entry(flake_root, relative, entry, state)
}

#[allow(clippy::too_many_arguments)]
fn store_content_entry(
    flake_root: &Utf8Path,
    relative: &str,
    file_identity: Option<FileIdentity>,
    size: u64,
    modified_ns: u128,
    changed_ns: Option<u128>,
    digest: String,
    state: &mut IncrementalDigestState,
) -> io::Result<()> {
    if !action_digest_index_enabled() {
        return Ok(());
    }
    let entry = ActionDigestEntry {
        file_identity,
        size,
        modified_ns,
        changed_ns,
        content_hash: digest,
        git_blob: None,
    };
    upsert_entry(flake_root, relative, entry, state)
}

fn prior_content_entry(
    flake_root: &Utf8Path,
    relative: &str,
    state: &mut IncrementalDigestState,
) -> io::Result<Option<ActionDigestEntry>> {
    if !action_digest_index_enabled() {
        return Ok(None);
    }
    let Some(session) = ensure_index_session(flake_root, state)? else {
        return Ok(None);
    };
    Ok(session.index.entries.get(relative).cloned())
}

fn upsert_entry(
    flake_root: &Utf8Path,
    relative: &str,
    entry: ActionDigestEntry,
    state: &mut IncrementalDigestState,
) -> io::Result<()> {
    let changed = {
        let Some(session) = ensure_index_session(flake_root, state)? else {
            return Ok(());
        };
        if session.index.entries.get(relative) == Some(&entry) {
            false
        } else {
            session.index.entries.insert(relative.to_owned(), entry);
            true
        }
    };
    if changed {
        state.dirty = true;
    }
    Ok(())
}

fn ensure_index_session<'a>(
    flake_root: &Utf8Path,
    state: &'a mut IncrementalDigestState,
) -> io::Result<Option<&'a mut IndexSession>> {
    if state.index.is_none() {
        let root = canonical_root(flake_root);
        let Some(path) = action_digest_index_path(&root) else {
            return Ok(None);
        };
        let loaded = load_action_digest_index(&path)?
            .filter(|index| index_compatible(index, &root))
            .unwrap_or_else(|| ActionDigestIndex {
                schema_version: ACTION_DIGEST_INDEX_SCHEMA_VERSION,
                root: root.as_str().to_owned(),
                entries: BTreeMap::new(),
            });
        state.index = Some(IndexSession {
            path,
            index: loaded.clone(),
            loaded,
        });
    }
    Ok(state.index.as_mut())
}

fn ensure_git_snapshot<'a>(
    flake_root: &Utf8Path,
    state: &'a mut IncrementalDigestState,
) -> io::Result<Option<&'a GitDigestSnapshot>> {
    if state.git.is_none() {
        state.git = Some(load_git_snapshot(flake_root)?);
    }
    Ok(state.git.as_ref().and_then(Option::as_ref))
}

fn load_git_snapshot(flake_root: &Utf8Path) -> io::Result<Option<GitDigestSnapshot>> {
    let inside = Command::new("git")
        .args([
            "-C",
            flake_root.as_str(),
            "rev-parse",
            "--is-inside-work-tree",
        ])
        .output();
    let Ok(inside) = inside else {
        return Ok(None);
    };
    if !inside.status.success() {
        return Ok(None);
    }
    let inside_text = String::from_utf8_lossy(&inside.stdout);
    if inside_text.trim() != "true" {
        return Ok(None);
    }

    let ls = Command::new("git")
        .args(["-C", flake_root.as_str(), "ls-files", "--stage", "-z"])
        .output()?;
    if !ls.status.success() {
        return Ok(None);
    }

    let status = Command::new("git")
        .args([
            "-C",
            flake_root.as_str(),
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .output()?;
    if !status.status.success() {
        return Ok(None);
    }

    let mut blobs = HashMap::new();
    for record in ls.stdout.split(|&b| b == 0) {
        if record.is_empty() {
            continue;
        }
        let Some((meta, path_bytes)) = split_once(record, b'\t') else {
            continue;
        };
        let meta = String::from_utf8_lossy(meta);
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let Some(oid) = parts.next() else {
            continue;
        };
        let Some(stage) = parts.next() else {
            continue;
        };
        if stage != "0" {
            // Merge conflict stages — force content hashing via dirty set.
            if let Ok(path) = std::str::from_utf8(path_bytes) {
                // Inserted below into dirty after parsing status; also mark here.
                let _ = path;
            }
            continue;
        }
        if let Ok(path) = std::str::from_utf8(path_bytes) {
            blobs.insert(path.replace('\\', "/"), oid.to_owned());
        }
    }

    let mut dirty_or_untracked = HashSet::new();
    // Also treat non-stage-0 paths as dirty.
    for record in ls.stdout.split(|&b| b == 0) {
        if record.is_empty() {
            continue;
        }
        let Some((meta, path_bytes)) = split_once(record, b'\t') else {
            continue;
        };
        let meta = String::from_utf8_lossy(meta);
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let _oid = parts.next();
        let Some(stage) = parts.next() else {
            continue;
        };
        if stage != "0"
            && let Ok(path) = std::str::from_utf8(path_bytes)
        {
            dirty_or_untracked.insert(path.replace('\\', "/"));
        }
    }

    parse_porcelain_z(&status.stdout, &mut dirty_or_untracked);

    Ok(Some(GitDigestSnapshot {
        blobs,
        dirty_or_untracked,
    }))
}

fn parse_porcelain_z(stdout: &[u8], out: &mut HashSet<String>) {
    let mut i = 0;
    while i < stdout.len() {
        if stdout[i] == 0 {
            i += 1;
            continue;
        }
        // Ordinary: XY SP path NUL  |  rename/copy: XY SP path NUL path NUL
        if i + 3 > stdout.len() {
            break;
        }
        // status is two chars then space (or rarely no space for some versions — require space)
        let rest = &stdout[i..];
        let Some(nul) = rest.iter().position(|&b| b == 0) else {
            break;
        };
        let entry = &rest[..nul];
        i += nul + 1;
        if entry.len() < 3 {
            continue;
        }
        // "XY path" — path starts at index 3 when XY + space
        let path_bytes = if entry.get(2) == Some(&b' ') {
            &entry[3..]
        } else {
            &entry[2..]
        };
        if let Ok(path) = std::str::from_utf8(path_bytes) {
            out.insert(path.replace('\\', "/"));
        }
        // Rename/copy: second path follows
        let xy0 = entry.first().copied().unwrap_or(0);
        if matches!(xy0, b'R' | b'C') {
            let Some(nul2) = stdout[i..].iter().position(|&b| b == 0) else {
                break;
            };
            let second = &stdout[i..i + nul2];
            i += nul2 + 1;
            if let Ok(path) = std::str::from_utf8(second) {
                out.insert(path.replace('\\', "/"));
            }
        }
    }
}

fn split_once(bytes: &[u8], sep: u8) -> Option<(&[u8], &[u8])> {
    let idx = bytes.iter().position(|&b| b == sep)?;
    Some((&bytes[..idx], &bytes[idx + 1..]))
}

fn index_compatible(index: &ActionDigestIndex, root: &Utf8Path) -> bool {
    index.schema_version == ACTION_DIGEST_INDEX_SCHEMA_VERSION && index.root == root.as_str()
}

fn canonical_root(flake_root: &Utf8Path) -> camino::Utf8PathBuf {
    flake_root
        .canonicalize()
        .ok()
        .and_then(|p| camino::Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| flake_root.to_path_buf())
}

fn action_digest_index_dir_inner() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(guard) = TEST_INDEX_ROOT.lock()
        && let Some(root) = guard.clone()
    {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("action-digests"))
}

/// Cache directory for action-digest indexes (separate from discovery fingerprint).
#[must_use]
pub fn action_digest_index_dir() -> Option<PathBuf> {
    action_digest_index_dir_inner()
}

fn action_digest_index_path(root: &Utf8Path) -> Option<PathBuf> {
    let index_root = action_digest_index_dir()?;
    let mut hasher = Hasher::new();
    hasher.update(root.as_str().as_bytes());
    Some(index_root.join(format!("{}.json", hasher.finalize().to_hex())))
}

fn load_action_digest_index(path: &Path) -> io::Result<Option<ActionDigestIndex>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match serde_json::from_str(&contents) {
        Ok(index) => Ok(Some(index)),
        Err(_) => Ok(None),
    }
}

fn store_action_digest_index(path: &Path, index: &ActionDigestIndex) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let serialized = serde_json::to_vec(index)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomically(path, &serialized)
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
            .unwrap_or("action-digest-index")
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
            .unwrap_or("action-digest-index"),
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    {
        let mut temp = File::create(&temp_path)?;
        temp.write_all(contents)?;
        temp.sync_all()?;
    }
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// Remove all durable action-digest index entries.
///
/// # Errors
///
/// Returns [`io::Error`] when entries cannot be removed.
pub fn clear_action_digest_index() -> io::Result<usize> {
    let Some(dir) = action_digest_index_dir() else {
        return Ok(0);
    };
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".json"))
        {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Status for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDigestIndexStatus {
    pub path: PathBuf,
    pub entries: usize,
    pub total_bytes: u64,
}

/// Summarize the action-digest index directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the directory cannot be read.
pub fn action_digest_index_status() -> io::Result<ActionDigestIndexStatus> {
    let path = action_digest_index_dir().unwrap_or_default();
    if !path.is_dir() {
        return Ok(ActionDigestIndexStatus {
            path,
            entries: 0,
            total_bytes: 0,
        });
    }
    let mut entries = 0;
    let mut total_bytes = 0;
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".json"))
        {
            entries += 1;
            total_bytes += meta.len();
        }
    }
    Ok(ActionDigestIndexStatus {
        path,
        entries,
        total_bytes,
    })
}

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

/// Override the action-digest index root for tests.
#[cfg(test)]
pub fn test_with_index_root<T>(root: PathBuf, f: impl FnOnce() -> T) -> T {
    let previous = {
        let mut guard = TEST_INDEX_ROOT.lock().expect("lock");
        (*guard).replace(root)
    };
    let result = f();
    let mut guard = TEST_INDEX_ROOT.lock().expect("lock");
    *guard = previous;
    result
}

/// Force Git digest enablement for tests (`None` = follow env).
#[cfg(test)]
pub fn test_with_git_digests<T>(enabled: Option<bool>, f: impl FnOnce() -> T) -> T {
    let previous = {
        let mut guard = TEST_GIT_DIGESTS.lock().expect("lock");
        let next = match enabled {
            None => TEST_FOLLOW_ENV,
            Some(false) => TEST_FORCE_OFF,
            Some(true) => TEST_FORCE_ON,
        };
        std::mem::replace(&mut *guard, next)
    };
    let result = f();
    *TEST_GIT_DIGESTS.lock().expect("lock") = previous;
    result
}

/// Force action-digest index enablement for tests (`None` = follow env).
#[cfg(test)]
pub fn test_with_action_digest_index<T>(enabled: Option<bool>, f: impl FnOnce() -> T) -> T {
    let previous = {
        let mut guard = TEST_ACTION_DIGEST_INDEX.lock().expect("lock");
        let next = match enabled {
            None => TEST_FOLLOW_ENV,
            Some(false) => TEST_FORCE_OFF,
            Some(true) => TEST_FORCE_ON,
        };
        std::mem::replace(&mut *guard, next)
    };
    let result = f();
    *TEST_ACTION_DIGEST_INDEX.lock().expect("lock") = previous;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf;
    use camino::Utf8PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn utf8_temp() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        (tmp, path)
    }

    fn git(args: &[&str], cwd: &Utf8Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "nxr-test")
            .env("GIT_AUTHOR_EMAIL", "nxr-test@example.com")
            .env("GIT_COMMITTER_NAME", "nxr-test")
            .env("GIT_COMMITTER_EMAIL", "nxr-test@example.com")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn blob_oid(cwd: &Utf8Path, path: &str) -> String {
        let output = Command::new("git")
            .args(["ls-files", "--stage", path])
            .current_dir(cwd)
            .output()
            .expect("ls-files");
        assert!(output.status.success());
        let line = String::from_utf8_lossy(&output.stdout);
        line.split_whitespace().nth(1).expect("oid").to_owned()
    }

    #[test]
    fn git_blob_domain_differs_from_content_hash() {
        let oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let from_blob = digest_from_git_blob(oid);
        let from_content = crate::cas::digest_bytes(b"not-the-blob");
        assert_ne!(from_blob, from_content);
        assert_eq!(from_blob, digest_from_git_blob(oid));
    }

    #[test]
    fn clean_tracked_uses_git_blob_without_rereading_bytes() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let index_home = tmp.path().join("index");
        test_with_index_root(index_home, || {
            git(&["init"], &root);
            let payload = vec![b'x'; 64_000];
            fs::write(root.join("tracked.bin"), &payload).expect("write");
            git(&["add", "tracked.bin"], &root);
            git(&["commit", "-m", "init"], &root);

            let oid = blob_oid(&root, "tracked.bin");
            let expected = {
                let mut hasher = Hasher::new();
                hasher.update(GIT_BLOB_DIGEST_DOMAIN);
                hasher.update(&[0]);
                hasher.update(oid.as_bytes());
                hasher.finalize().to_hex().to_string()
            };

            perf::test_reset(true);
            let mut state = IncrementalDigestState::new();
            let digest =
                digest_repo_path_incremental(&root, "tracked.bin", &mut state).expect("digest");
            assert_eq!(digest, expected);

            let stats = perf::PerfStats::snapshot();
            // Domain string only — not the 64 KiB payload.
            assert!(stats.bytes_hashed < 1_000);
            assert_eq!(stats.git_blob_digests, 1);
        });
    }

    #[test]
    fn dirty_tracked_hashes_working_tree_content() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let index_home = tmp.path().join("index");
        test_with_index_root(index_home, || {
            git(&["init"], &root);
            fs::write(root.join("file.txt"), b"clean").expect("write");
            git(&["add", "file.txt"], &root);
            git(&["commit", "-m", "init"], &root);
            fs::write(root.join("file.txt"), b"dirty-content").expect("dirty");

            let mut state = IncrementalDigestState::new();
            let digest =
                digest_repo_path_incremental(&root, "file.txt", &mut state).expect("digest");
            let content = digest_file(root.join("file.txt").as_std_path()).expect("content");
            assert_eq!(digest, content);
            assert_ne!(digest, digest_from_git_blob(&blob_oid(&root, "file.txt")));
        });
    }

    #[test]
    fn untracked_hashes_content() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let index_home = tmp.path().join("index");
        test_with_index_root(index_home, || {
            git(&["init"], &root);
            fs::write(root.join("keep.txt"), b"tracked").expect("write");
            git(&["add", "keep.txt"], &root);
            git(&["commit", "-m", "init"], &root);
            fs::write(root.join("new.txt"), b"untracked-bytes").expect("write");

            let mut state = IncrementalDigestState::new();
            let digest =
                digest_repo_path_incremental(&root, "new.txt", &mut state).expect("digest");
            let content = digest_file(root.join("new.txt").as_std_path()).expect("content");
            assert_eq!(digest, content);
        });
    }

    #[test]
    fn metadata_gate_skips_content_reread() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let index_home = tmp.path().join("index");
        test_with_git_digests(Some(false), || {
            test_with_index_root(index_home, || {
                let payload = vec![b'y'; 32_000];
                fs::write(root.join("data.bin"), &payload).expect("write");

                {
                    let mut state = IncrementalDigestState::new();
                    let _ = digest_repo_path_incremental(&root, "data.bin", &mut state).expect("1");
                    state.flush().expect("flush");
                }

                perf::test_reset(true);
                let mut state = IncrementalDigestState::new();
                let digest =
                    digest_repo_path_incremental(&root, "data.bin", &mut state).expect("2");
                let stats = perf::PerfStats::snapshot();
                assert_eq!(stats.digest_metadata_hits, 1);
                assert!(stats.bytes_hashed < 1_000, "should not re-hash 32KiB");
                // Verify without recording bytes_hashed on the hot path under test.
                perf::test_reset(false);
                let expected = digest_file(root.join("data.bin").as_std_path()).expect("content");
                assert_eq!(digest, expected);
            });
        });
    }

    #[test]
    fn kill_switch_matches_content_digest_repo_path() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let index_home = tmp.path().join("index");
        test_with_git_digests(Some(false), || {
            test_with_action_digest_index(Some(false), || {
                test_with_index_root(index_home, || {
                    git(&["init"], &root);
                    fs::write(root.join("a.txt"), b"same-tree").expect("write");
                    git(&["add", "a.txt"], &root);
                    git(&["commit", "-m", "init"], &root);

                    let mut state = IncrementalDigestState::new();
                    let incremental =
                        digest_repo_path_incremental(&root, "a.txt", &mut state).expect("inc");
                    let baseline = crate::cas::digest_repo_path(&root, "a.txt").expect("base");
                    assert_eq!(incremental, baseline);
                });
            });
        });
    }
}
