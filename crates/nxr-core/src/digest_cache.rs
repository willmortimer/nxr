//! Per-invocation memo for workspace path digests ([ADR-0154]).
//!
//! Action-key construction may hash the same repo-relative paths many times when
//! task inputs overlap. [`RunDigestCache`] deduplicates file reads and BLAKE3
//! work within one CLI invocation. It is in-memory only; persistent Merkle
//! indexing is a separate concern (perf Wave 3 / 2b).

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use crate::cas::{digest_file, digest_repo_path, flake_lock_digest};
use crate::perf::record_digest_cache_hit;

/// Run-scoped digest memo for workspace action keys.
///
/// Create one instance per `nxr task` / affected / action-key planning pass and
/// pass it through [`build_workspace_cache_plan`](nxr_task::build_workspace_cache_plan)
/// (or call [`digest_repo_path`](Self::digest_repo_path) directly). Wave 2b may
/// extend this with Git blob identity without changing call sites.
#[derive(Clone, Debug, Default)]
pub struct RunDigestCache {
    repo_files: Option<Vec<Utf8PathBuf>>,
    path_digests: HashMap<String, String>,
    pattern_digests: HashMap<String, BTreeMap<String, String>>,
    hits: u64,
    misses: u64,
}

impl RunDigestCache {
    /// Empty cache for one invocation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cache hits (path or pattern reuse).
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cache misses (first digest or expansion for a key).
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Digest a repo-relative path, reusing prior results within this run.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when traversal or reading fails.
    pub fn digest_repo_path(
        &mut self,
        flake_root: &Utf8Path,
        relative: &str,
    ) -> io::Result<String> {
        if let Some(digest) = self.path_digests.get(relative) {
            self.hits += 1;
            record_digest_cache_hit();
            return Ok(digest.clone());
        }
        self.misses += 1;
        let digest = digest_repo_path(flake_root, relative)?;
        self.path_digests
            .insert(relative.to_owned(), digest.clone());
        Ok(digest)
    }

    /// Digest `flake.lock` when present, memoized by path key `flake.lock`.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the lockfile cannot be read.
    pub fn flake_lock_digest(&mut self, flake_root: &Utf8Path) -> io::Result<Option<String>> {
        const KEY: &str = "flake.lock";
        if let Some(digest) = self.path_digests.get(KEY) {
            self.hits += 1;
            record_digest_cache_hit();
            return Ok(Some(digest.clone()));
        }
        let digest = flake_lock_digest(flake_root)?;
        if let Some(digest) = digest {
            self.path_digests.insert(KEY.to_owned(), digest.clone());
            self.misses += 1;
            Ok(Some(digest))
        } else {
            Ok(None)
        }
    }

    /// Digest `flake.nix` when present, memoized by path key `flake.nix`.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the file cannot be read.
    pub fn flake_nix_digest(&mut self, flake_root: &Utf8Path) -> io::Result<Option<String>> {
        const KEY: &str = "flake.nix";
        if let Some(digest) = self.path_digests.get(KEY) {
            self.hits += 1;
            record_digest_cache_hit();
            return Ok(Some(digest.clone()));
        }
        let flake_nix = flake_root.join("flake.nix");
        if !flake_nix.is_file() {
            return Ok(None);
        }
        self.misses += 1;
        let digest = digest_file(flake_nix.as_std_path())?;
        self.path_digests.insert(KEY.to_owned(), digest.clone());
        Ok(Some(digest))
    }

    /// Sorted repo-relative file paths under `flake_root` (`.git` skipped).
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the tree cannot be walked.
    pub fn repo_files(&mut self, flake_root: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
        if let Some(files) = &self.repo_files {
            self.hits += 1;
            record_digest_cache_hit();
            return Ok(files.clone());
        }
        self.misses += 1;
        let files = walk_repo_files(flake_root)?;
        self.repo_files = Some(files.clone());
        Ok(files)
    }

    /// Prior expansion for a normalized `inputs.paths` glob or literal.
    pub fn pattern_digests(&mut self, pattern: &str) -> Option<BTreeMap<String, String>> {
        if let Some(digests) = self.pattern_digests.get(pattern) {
            self.hits += 1;
            record_digest_cache_hit();
            return Some(digests.clone());
        }
        None
    }

    /// Store expansion digests for reuse when another task shares the pattern.
    pub fn store_pattern_digests(&mut self, pattern: String, digests: BTreeMap<String, String>) {
        self.pattern_digests.insert(pattern, digests);
    }
}

fn walk_repo_files(flake_root: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
    let mut files = Vec::new();
    collect_repo_files(flake_root.as_std_path(), &mut files)?;
    files.sort();
    Ok(files
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(flake_root.as_std_path())
                .ok()
                .and_then(|relative| Utf8PathBuf::from_path_buf(relative.to_path_buf()).ok())
        })
        .collect())
}

fn collect_repo_files(current: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if current.is_file() {
        out.push(current.to_path_buf());
        return Ok(());
    }
    if !current.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == OsStr::new(".git") {
            continue;
        }
        collect_repo_files(&path, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_repo_path_hashes_content_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let content = b"repeat-me-content-for-digest-cache";
        std::fs::write(flake.join("input.txt"), content).expect("write");

        let mut cache = RunDigestCache::new();
        let first = cache
            .digest_repo_path(&flake, "input.txt")
            .expect("first digest");
        let second = cache
            .digest_repo_path(&flake, "input.txt")
            .expect("second digest");

        assert_eq!(first, second);
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn repo_files_walk_once_per_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        std::fs::create_dir_all(flake.join("a")).expect("mkdir");
        std::fs::write(flake.join("a/one"), b"1").expect("write");
        std::fs::write(flake.join("a/two"), b"2").expect("write");

        let mut cache = RunDigestCache::new();
        let first = cache.repo_files(&flake).expect("walk");
        let second = cache.repo_files(&flake).expect("walk again");
        assert_eq!(first, second);
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }
}
