//! Repository Merkle / directory digest index ([ADR-0156]).
//!
//! Directory digests are aggregated from immediate children so a directory
//! digest changes only when a descendant changes. Leaf digests are Wave 2b
//! per-file action digests (Git blob–mapped or content-hashed) via
//! [`crate::incremental_digest`].
//!
//! On-disk state is separate from discovery fingerprint and action-digest
//! indexes. Kill-switch: [`MERKLE_INDEX_ENV`].
//!
//! # Wave 4 / watch hooks
//!
//! [`invalidate_paths`] drops durable digests for changed leaves and their
//! ancestor directories so a future `nxrd` or watch snapshot (Wave 5) can reload
//! this index and recompute only dirty subtrees. Full FS watch integration is
//! intentionally not implemented here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use blake3::Hasher;
use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::sync::Mutex;

use crate::cas::{ensure_within_workspace, missing_path_digest};
use crate::incremental_digest::{IncrementalDigestState, digest_file_for_merkle};
use crate::perf::add_bytes_hashed;

/// Kill-switch for the durable Merkle / directory index (`off` / `0` / `false` / `no`).
pub const MERKLE_INDEX_ENV: &str = "NXR_MERKLE_INDEX";

/// On-disk Merkle index schema version.
pub const MERKLE_INDEX_SCHEMA_VERSION: u32 = 1;

/// Domain tag for directory aggregation (action-key material when Merkle is on).
///
/// ```text
/// digest = BLAKE3(
///   "nxr.merkle.dir.v1" ‖ 0x00 ‖
///   for each child in sorted name order:
///     name ‖ 0x00 ‖ kind ‖ 0x00 ‖ child_digest
/// )
/// ```
/// where `kind` is `f` (file) or `d` (directory).
pub const MERKLE_DIR_DOMAIN: &[u8] = b"nxr.merkle.dir.v1";

/// Documents that leaves reuse Wave 2b action-digest material (not pure CAS).
pub const MERKLE_LEAF_KIND: &str = "action-digest-v1";

#[cfg(test)]
static TEST_MERKLE_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);
#[cfg(test)]
static TEST_MERKLE_INDEX: Mutex<u8> = Mutex::new(0);

#[cfg(test)]
const TEST_FOLLOW_ENV: u8 = 0;
#[cfg(test)]
const TEST_FORCE_OFF: u8 = 1;
#[cfg(test)]
const TEST_FORCE_ON: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChildKind {
    File,
    Dir,
}

impl ChildKind {
    fn tag(self) -> u8 {
        match self {
            Self::File => b'f',
            Self::Dir => b'd',
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct MerkleChild {
    kind: ChildKind,
    digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct MerkleDirEntry {
    digest: String,
    children: BTreeMap<String, MerkleChild>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct MerkleIndexFile {
    schema_version: u32,
    root: String,
    /// Always [`MERKLE_LEAF_KIND`] for schema v1.
    leaf_kind: String,
    dirs: BTreeMap<String, MerkleDirEntry>,
}

/// In-memory session over one durable Merkle index (one flake root per run).
#[derive(Debug)]
pub struct MerkleSession {
    path: PathBuf,
    index: MerkleIndexFile,
    loaded: MerkleIndexFile,
    dirty: bool,
    /// Dir keys computed in this process (disk entries are not trusted until rebuilt
    /// or invalidated by [`invalidate_paths`] in a long-lived daemon).
    computed: BTreeSet<String>,
}

impl MerkleSession {
    /// Persist updates if any.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the index cannot be written.
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.dirty || self.index == self.loaded {
            return Ok(());
        }
        store_merkle_index(&self.path, &self.index)?;
        self.loaded = self.index.clone();
        self.dirty = false;
        Ok(())
    }

    /// Directory digest for a repo-relative path (`""` = repository root).
    #[must_use]
    pub fn dir_digest(&self, relative: &str) -> Option<&str> {
        let key = normalize_dir_key(relative);
        self.index.dirs.get(&key).map(|entry| entry.digest.as_str())
    }
}

impl Drop for MerkleSession {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Whether the durable Merkle index is enabled (default on).
#[must_use]
pub fn merkle_index_enabled() -> bool {
    #[cfg(test)]
    if let Ok(guard) = TEST_MERKLE_INDEX.lock() {
        match *guard {
            TEST_FORCE_OFF => return false,
            TEST_FORCE_ON => return true,
            _ => {}
        }
    }
    env_flag_enabled(MERKLE_INDEX_ENV, true)
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

/// Aggregate immediate children into a domain-separated directory digest.
fn aggregate_dir_digest(children: &BTreeMap<String, MerkleChild>) -> String {
    let mut hasher = Hasher::new();
    hasher.update(MERKLE_DIR_DOMAIN);
    hasher.update(&[0]);
    let mut domain_bytes = MERKLE_DIR_DOMAIN.len() as u64 + 1;
    for (name, child) in children {
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hasher.update(&[child.kind.tag()]);
        hasher.update(&[0]);
        hasher.update(child.digest.as_bytes());
        domain_bytes += name.len() as u64 + 1 + 1 + 1 + child.digest.len() as u64;
    }
    add_bytes_hashed(domain_bytes);
    hasher.finalize().to_hex().to_string()
}

/// Ancestor directories (including `""` for repo root) touched by `changed_paths`.
///
/// Used by affected analysis locality and watch invalidation planning. Does not
/// require a loaded index — mirrors Merkle ancestor walks from path prefixes.
#[must_use]
pub fn touched_directories(changed_paths: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for path in changed_paths {
        let normalized = normalize_repo_rel(path);
        if normalized.is_empty() {
            out.insert(String::new());
            continue;
        }
        // File or dir path: every ancestor directory key.
        let mut current = normalized.as_str();
        while let Some((parent, _)) = current.rsplit_once('/') {
            out.insert(parent.to_owned());
            current = parent;
        }
        out.insert(String::new());
        // If the change names a directory itself, include it.
        out.insert(normalized);
    }
    out
}

/// Drop digests for `paths` and ancestor directories (Wave 4 / watch hook).
///
/// After invalidation, the next [`digest_dir_merkle`] rebuilds only missing
/// subtrees. Safe no-op when Merkle is disabled or the session is absent.
pub fn invalidate_paths(session: &mut MerkleSession, paths: &[String]) {
    let touched = touched_directories(paths);
    let mut changed = false;
    for key in &touched {
        session.computed.remove(key);
        if session.index.dirs.remove(key).is_some() {
            changed = true;
        }
    }
    for path in paths {
        let file_key = normalize_repo_rel(path);
        session.computed.remove(&file_key);
        // Clearing a dir entry that matched the path exactly (directory rename).
        if session.index.dirs.remove(&file_key).is_some() {
            changed = true;
        }
    }
    if changed {
        session.dirty = true;
    }
}

/// Digest a directory via the Merkle index, building/updating as needed.
///
/// # Errors
///
/// Returns [`io::Error`] when traversal or leaf digesting fails.
pub fn digest_dir_merkle(
    flake_root: &Utf8Path,
    relative: &str,
    state: &mut IncrementalDigestState,
) -> io::Result<String> {
    let root = flake_root.as_std_path();
    let path = if relative.is_empty() || relative == "." {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    if !path.exists() {
        return Ok(missing_path_digest());
    }
    ensure_within_workspace(root, &path)?;
    if path.is_file() {
        return digest_file_for_merkle(flake_root, relative, &path, state);
    }
    let key = normalize_dir_key(relative);
    compute_dir(flake_root, &key, &path, state)
}

fn compute_dir(
    flake_root: &Utf8Path,
    dir_key: &str,
    abs_dir: &Path,
    state: &mut IncrementalDigestState,
) -> io::Result<String> {
    // In-session memo: after invalidate_paths, unrelated dirs keep their digests
    // and skip re-walk (edit locality). Disk entries alone are not trusted on a
    // cold CLI process — they are rebuilt once, then memoized for the session.
    let cached = state.merkle_session().and_then(|session| {
        if session.computed.contains(dir_key) {
            session
                .index
                .dirs
                .get(dir_key)
                .map(|entry| entry.digest.clone())
        } else {
            None
        }
    });
    if let Some(digest) = cached {
        return Ok(digest);
    }

    let mut children = BTreeMap::new();
    let root = flake_root.as_std_path();
    for entry in fs::read_dir(abs_dir)? {
        let entry = entry?;
        let name_os = entry.file_name();
        if name_os == *".git" {
            continue;
        }
        let name = name_os.to_string_lossy().replace('\\', "/");
        if name.contains('/') {
            continue;
        }
        let child_path = entry.path();
        let child_rel = if dir_key.is_empty() {
            name.clone()
        } else {
            format!("{dir_key}/{name}")
        };
        ensure_within_workspace(root, &child_path)?;
        let meta = entry.metadata()?;
        if meta.file_type().is_symlink() {
            // Match cas::collect_files: follow only when the target stays in-tree.
            let resolved = match child_path.canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => continue,
            };
            if !resolved.starts_with(root) {
                continue;
            }
            if resolved.is_dir() {
                let digest = compute_dir(flake_root, &child_rel, &resolved, state)?;
                children.insert(
                    name,
                    MerkleChild {
                        kind: ChildKind::Dir,
                        digest,
                    },
                );
            } else if resolved.is_file() {
                let digest = digest_file_for_merkle(flake_root, &child_rel, &resolved, state)?;
                children.insert(
                    name,
                    MerkleChild {
                        kind: ChildKind::File,
                        digest,
                    },
                );
            }
            continue;
        }
        if child_path.is_dir() {
            let digest = compute_dir(flake_root, &child_rel, &child_path, state)?;
            children.insert(
                name,
                MerkleChild {
                    kind: ChildKind::Dir,
                    digest,
                },
            );
        } else if child_path.is_file() {
            let digest = digest_file_for_merkle(flake_root, &child_rel, &child_path, state)?;
            children.insert(
                name,
                MerkleChild {
                    kind: ChildKind::File,
                    digest,
                },
            );
        }
    }

    let digest = aggregate_dir_digest(&children);
    if let Some(session) = ensure_merkle_session(flake_root, state)? {
        let entry = MerkleDirEntry {
            digest: digest.clone(),
            children,
        };
        if session.index.dirs.get(dir_key) != Some(&entry) {
            session.index.dirs.insert(dir_key.to_owned(), entry);
            session.dirty = true;
        }
        session.computed.insert(dir_key.to_owned());
    }
    Ok(digest)
}

/// Ensure a Merkle session is loaded for `flake_root` (Wave 4: daemon can share).
///
/// # Errors
///
/// Returns [`io::Error`] when the index cannot be read.
pub fn ensure_merkle_session<'a>(
    flake_root: &Utf8Path,
    state: &'a mut IncrementalDigestState,
) -> io::Result<Option<&'a mut MerkleSession>> {
    if !merkle_index_enabled() {
        return Ok(None);
    }
    if state.merkle_session().is_none() {
        let root = canonical_root(flake_root);
        let Some(path) = merkle_index_path(&root) else {
            return Ok(None);
        };
        let loaded = load_merkle_index(&path)?
            .filter(|index| index_compatible(index, &root))
            .unwrap_or_else(|| MerkleIndexFile {
                schema_version: MERKLE_INDEX_SCHEMA_VERSION,
                root: root.as_str().to_owned(),
                leaf_kind: MERKLE_LEAF_KIND.to_owned(),
                dirs: BTreeMap::new(),
            });
        state.set_merkle_session(MerkleSession {
            path,
            index: loaded.clone(),
            loaded,
            dirty: false,
            computed: BTreeSet::new(),
        });
    }
    Ok(state.merkle_session_mut())
}

fn index_compatible(index: &MerkleIndexFile, root: &Utf8Path) -> bool {
    index.schema_version == MERKLE_INDEX_SCHEMA_VERSION
        && index.root == root.as_str()
        && index.leaf_kind == MERKLE_LEAF_KIND
}

fn canonical_root(flake_root: &Utf8Path) -> Utf8PathBuf {
    flake_root
        .canonicalize()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| flake_root.to_path_buf())
}

fn normalize_repo_rel(path: &str) -> String {
    let trimmed = path.trim().trim_start_matches("./");
    trimmed.replace('\\', "/")
}

fn normalize_dir_key(relative: &str) -> String {
    let normalized = normalize_repo_rel(relative);
    if normalized.is_empty() || normalized == "." {
        String::new()
    } else {
        normalized.trim_end_matches('/').to_owned()
    }
}

fn merkle_index_dir_inner() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(guard) = TEST_MERKLE_ROOT.lock()
        && let Some(root) = guard.clone()
    {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.cache_dir().join("merkle-index"))
}

/// Cache directory for Merkle indexes (separate from action-digests / discovery).
#[must_use]
pub fn merkle_index_dir() -> Option<PathBuf> {
    merkle_index_dir_inner()
}

fn merkle_index_path(root: &Utf8Path) -> Option<PathBuf> {
    let index_root = merkle_index_dir()?;
    let mut hasher = Hasher::new();
    hasher.update(root.as_str().as_bytes());
    Some(index_root.join(format!("{}.json", hasher.finalize().to_hex())))
}

fn load_merkle_index(path: &Path) -> io::Result<Option<MerkleIndexFile>> {
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

fn store_merkle_index(path: &Path, index: &MerkleIndexFile) -> io::Result<()> {
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
            .unwrap_or("merkle-index")
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
            .unwrap_or("merkle-index"),
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

/// Remove all durable Merkle index entries.
///
/// # Errors
///
/// Returns [`io::Error`] when entries cannot be removed.
pub fn clear_merkle_index() -> io::Result<usize> {
    let Some(dir) = merkle_index_dir() else {
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
pub struct MerkleIndexStatus {
    pub path: PathBuf,
    pub entries: usize,
    pub total_bytes: u64,
}

/// Summarize the Merkle index directory.
///
/// # Errors
///
/// Returns [`io::Error`] when the directory cannot be read.
pub fn merkle_index_status() -> io::Result<MerkleIndexStatus> {
    let path = merkle_index_dir().unwrap_or_default();
    if !path.is_dir() {
        return Ok(MerkleIndexStatus {
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
    Ok(MerkleIndexStatus {
        path,
        entries,
        total_bytes,
    })
}

/// Override the Merkle index root for tests.
#[cfg(test)]
pub fn test_with_merkle_root<T>(root: PathBuf, f: impl FnOnce() -> T) -> T {
    let previous = {
        let mut guard = TEST_MERKLE_ROOT.lock().expect("lock");
        (*guard).replace(root)
    };
    let result = f();
    let mut guard = TEST_MERKLE_ROOT.lock().expect("lock");
    *guard = previous;
    result
}

/// Force Merkle index enablement for tests (`None` = follow env).
#[cfg(test)]
pub fn test_with_merkle_index<T>(enabled: Option<bool>, f: impl FnOnce() -> T) -> T {
    let previous = {
        let mut guard = TEST_MERKLE_INDEX.lock().expect("lock");
        let next = match enabled {
            None => TEST_FOLLOW_ENV,
            Some(false) => TEST_FORCE_OFF,
            Some(true) => TEST_FORCE_ON,
        };
        std::mem::replace(&mut *guard, next)
    };
    let result = f();
    *TEST_MERKLE_INDEX.lock().expect("lock") = previous;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::digest_repo_path;
    use crate::incremental_digest::{
        digest_repo_path_incremental, test_with_action_digest_index, test_with_git_digests,
        test_with_index_root,
    };
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

    fn write_tree(root: &Utf8Path) {
        fs::create_dir_all(root.join("apps/foo")).expect("mkdir");
        fs::create_dir_all(root.join("apps/bar")).expect("mkdir");
        fs::write(root.join("apps/foo/a.txt"), b"foo-a").expect("write");
        fs::write(root.join("apps/foo/b.txt"), b"foo-b").expect("write");
        fs::write(root.join("apps/bar/c.txt"), b"bar-c").expect("write");
        fs::write(root.join("root.txt"), b"root").expect("write");
    }

    #[test]
    fn edit_locality_unrelated_dir_digest_stable() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let merkle_home = tmp.path().join("merkle");
        let action_home = tmp.path().join("action");
        write_tree(&root);

        test_with_merkle_index(Some(true), || {
            test_with_merkle_root(merkle_home.clone(), || {
                test_with_git_digests(Some(false), || {
                    test_with_action_digest_index(Some(false), || {
                        test_with_index_root(action_home.clone(), || {
                            let mut state = IncrementalDigestState::new();
                            let foo_before =
                                digest_repo_path_incremental(&root, "apps/foo", &mut state)
                                    .expect("foo");
                            let bar_before =
                                digest_repo_path_incremental(&root, "apps/bar", &mut state)
                                    .expect("bar");

                            // Invalidate so the next pass rebuilds apps/foo only.
                            if let Some(session) = state.merkle_session_mut() {
                                invalidate_paths(session, &["apps/foo/a.txt".to_owned()]);
                            }
                            fs::write(root.join("apps/foo/a.txt"), b"foo-a-changed").expect("edit");

                            let foo_after =
                                digest_repo_path_incremental(&root, "apps/foo", &mut state)
                                    .expect("foo2");
                            let bar_after =
                                digest_repo_path_incremental(&root, "apps/bar", &mut state)
                                    .expect("bar2");

                            assert_ne!(foo_before, foo_after);
                            assert_eq!(bar_before, bar_after);
                        });
                    });
                });
            });
        });
    }

    #[test]
    fn rename_move_updates_source_and_dest_dirs() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let merkle_home = tmp.path().join("merkle");
        let action_home = tmp.path().join("action");
        write_tree(&root);

        test_with_merkle_index(Some(true), || {
            test_with_merkle_root(merkle_home, || {
                test_with_git_digests(Some(false), || {
                    test_with_action_digest_index(Some(false), || {
                        test_with_index_root(action_home, || {
                            let mut state = IncrementalDigestState::new();
                            let foo_before =
                                digest_repo_path_incremental(&root, "apps/foo", &mut state)
                                    .expect("foo");
                            let bar_before =
                                digest_repo_path_incremental(&root, "apps/bar", &mut state)
                                    .expect("bar");

                            if let Some(session) = state.merkle_session_mut() {
                                invalidate_paths(
                                    session,
                                    &["apps/foo/b.txt".to_owned(), "apps/bar/b.txt".to_owned()],
                                );
                            }
                            fs::rename(root.join("apps/foo/b.txt"), root.join("apps/bar/b.txt"))
                                .expect("rename");

                            let foo_after =
                                digest_repo_path_incremental(&root, "apps/foo", &mut state)
                                    .expect("foo2");
                            let bar_after =
                                digest_repo_path_incremental(&root, "apps/bar", &mut state)
                                    .expect("bar2");

                            assert_ne!(foo_before, foo_after);
                            assert_ne!(bar_before, bar_after);
                        });
                    });
                });
            });
        });
    }

    #[test]
    fn kill_switch_off_matches_prior_flat_directory_digest() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let merkle_home = tmp.path().join("merkle");
        let action_home = tmp.path().join("action");
        write_tree(&root);

        test_with_merkle_index(Some(false), || {
            test_with_merkle_root(merkle_home, || {
                test_with_git_digests(Some(false), || {
                    test_with_action_digest_index(Some(false), || {
                        test_with_index_root(action_home, || {
                            let mut state = IncrementalDigestState::new();
                            let incremental =
                                digest_repo_path_incremental(&root, "apps/foo", &mut state)
                                    .expect("inc");
                            let baseline = digest_repo_path(&root, "apps/foo").expect("baseline");
                            assert_eq!(incremental, baseline);
                        });
                    });
                });
            });
        });
    }

    #[test]
    fn touched_directories_includes_ancestors() {
        let touched = touched_directories(&["apps/foo/a.txt".to_owned()]);
        assert!(touched.contains(""));
        assert!(touched.contains("apps"));
        assert!(touched.contains("apps/foo"));
        assert!(touched.contains("apps/foo/a.txt"));
    }

    #[test]
    fn large_tree_dir_digest_is_stable_and_local() {
        let _guard = test_lock().lock().expect("lock");
        let (tmp, root) = utf8_temp();
        let merkle_home = tmp.path().join("merkle");
        let action_home = tmp.path().join("action");

        // Bounded synthetic tree (~200 files) for locality / stability.
        for i in 0..20 {
            let dir = root.join(format!("pkg{i:02}"));
            fs::create_dir_all(&dir).expect("mkdir");
            for j in 0..10 {
                fs::write(dir.join(format!("f{j}.txt")), format!("pkg{i}-{j}")).expect("write");
            }
        }

        test_with_merkle_index(Some(true), || {
            test_with_merkle_root(merkle_home, || {
                test_with_git_digests(Some(false), || {
                    test_with_action_digest_index(Some(false), || {
                        test_with_index_root(action_home, || {
                            let mut state = IncrementalDigestState::new();
                            let d00 = digest_repo_path_incremental(&root, "pkg00", &mut state)
                                .expect("pkg00");
                            let d19 = digest_repo_path_incremental(&root, "pkg19", &mut state)
                                .expect("pkg19");

                            if let Some(session) = state.merkle_session_mut() {
                                invalidate_paths(session, &["pkg00/f0.txt".to_owned()]);
                            }
                            fs::write(root.join("pkg00/f0.txt"), b"changed").expect("edit");

                            let d00_after =
                                digest_repo_path_incremental(&root, "pkg00", &mut state)
                                    .expect("pkg00b");
                            let d19_after =
                                digest_repo_path_incremental(&root, "pkg19", &mut state)
                                    .expect("pkg19b");

                            assert_ne!(d00, d00_after);
                            assert_eq!(d19, d19_after);
                        });
                    });
                });
            });
        });
    }
}
