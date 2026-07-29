//! Shared on-disk JSON cache status, GC, and selective invalidation helpers.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::record_fs_metadata;

/// Hex-encoded BLAKE3 digest length used for timed JSON cache file stems.
const BLAKE3_HEX_LEN: usize = 64;

/// Returns whether `stem` is safe to use as a timed JSON cache file stem.
///
/// Accepts fixed-length lowercase hex digests (BLAKE3) or a conservative
/// filename alphabet for discovery-style stems (`[a-z0-9_-]`).
#[must_use]
pub fn is_valid_timed_json_entry_stem(stem: &str) -> bool {
    if stem.is_empty() {
        return false;
    }
    if stem.len() == BLAKE3_HEX_LEN
        && stem
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'f' | b'0'..=b'9'))
    {
        return true;
    }
    stem.bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
}

/// Aggregate counts and entry ages for a directory of timestamped JSON cache files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedDiskCacheSummary {
    pub entries: usize,
    pub total_bytes: u64,
    pub oldest_age_secs: Option<u64>,
    pub newest_age_secs: Option<u64>,
}

/// Summarize `.json` files under `root`, extracting recorded timestamps from contents.
///
/// `extract_recorded_at` returns the entry's recorded epoch seconds when parseable.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn summarize_timed_json_cache(
    root: &Path,
    now_secs: u64,
    extract_recorded_at: impl Fn(&str) -> Option<u64>,
) -> io::Result<TimedDiskCacheSummary> {
    if !root.is_dir() {
        return Ok(TimedDiskCacheSummary {
            entries: 0,
            total_bytes: 0,
            oldest_age_secs: None,
            newest_age_secs: None,
        });
    }

    let mut entries = 0usize;
    let mut total_bytes = 0u64;
    let mut oldest_age: Option<u64> = None;
    let mut newest_age: Option<u64> = None;

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        record_fs_metadata();
        total_bytes += entry.metadata()?.len();
        entries += 1;
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(recorded_at) = extract_recorded_at(&contents) else {
            continue;
        };
        let age = now_secs.saturating_sub(recorded_at);
        oldest_age = Some(oldest_age.map_or(age, |current| current.max(age)));
        newest_age = Some(newest_age.map_or(age, |current| current.min(age)));
    }

    Ok(TimedDiskCacheSummary {
        entries,
        total_bytes,
        oldest_age_secs: oldest_age,
        newest_age_secs: newest_age,
    })
}

/// Remove `.json` / `.tmp` files whose recorded timestamp exceeds `ttl_secs`.
///
/// Returns the number of files removed. When `ttl_secs` is `None`, no files are removed.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or entries cannot be removed.
pub fn prune_timed_json_cache(
    root: &Path,
    now_secs: u64,
    ttl_secs: Option<u64>,
    extract_recorded_at: impl Fn(&str) -> Option<u64>,
) -> io::Result<usize> {
    let Some(ttl) = ttl_secs else {
        return Ok(0);
    };
    if !root.is_dir() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file()
            || path
                .extension()
                .is_none_or(|ext| ext != "json" && ext != "tmp")
        {
            continue;
        }
        let expired = if path.extension().is_some_and(|ext| ext == "tmp") {
            true
        } else {
            let Ok(contents) = fs::read_to_string(&path) else {
                removed += usize::from(remove_cache_file(&path)?);
                continue;
            };
            match extract_recorded_at(&contents) {
                Some(recorded_at) => now_secs.saturating_sub(recorded_at) > ttl,
                None => true,
            }
        };
        if expired {
            removed += usize::from(remove_cache_file(&path)?);
        }
    }
    Ok(removed)
}

/// Remove a single `{stem}.json` entry (and any sibling `.tmp`) when present.
///
/// Returns `true` when a JSON entry was removed.
///
/// # Errors
///
/// Returns [`io::Error`] when `stem` is invalid, escapes `root`, or files cannot be removed.
pub fn remove_timed_json_entry(root: &Path, stem: &str) -> io::Result<bool> {
    if !is_valid_timed_json_entry_stem(stem) {
        return Err(invalid_stem_error(stem));
    }

    let json = timed_json_entry_path(root, stem, "json")?;
    let tmp = timed_json_entry_path(root, stem, "tmp")?;
    let mut removed = false;
    if json.is_file() {
        fs::remove_file(&json)?;
        removed = true;
    }
    if tmp.is_file() {
        fs::remove_file(&tmp)?;
    }
    Ok(removed)
}

fn timed_json_entry_path(root: &Path, stem: &str, ext: &str) -> io::Result<PathBuf> {
    let path = root.join(format!("{stem}.{ext}"));
    ensure_within_root(root, &path)
}

fn ensure_within_root(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let canonical_root = root.canonicalize().map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cache root `{}` cannot be canonicalized: {source}",
                root.display()
            ),
        )
    })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if let Ok(canonical) = absolute.canonicalize() {
        if !canonical.starts_with(&canonical_root) {
            return Err(cache_escape_error(root, path));
        }
        return Ok(canonical);
    }
    let relative = absolute.strip_prefix(root).unwrap_or(absolute.as_path());
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(cache_escape_error(root, path));
    }
    Ok(absolute)
}

fn invalid_stem_error(stem: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid timed JSON cache entry stem `{stem}`"),
    )
}

fn cache_escape_error(root: &Path, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "path `{}` escapes cache root `{}`",
            path.display(),
            root.display()
        ),
    )
}

fn remove_cache_file(path: &Path) -> io::Result<bool> {
    if path.is_file() {
        fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Unix seconds since epoch (shared by cache modules).
#[must_use]
pub fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn summarize_and_prune_respect_ttl() {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("cache");
        fs::create_dir_all(&root).expect("mkdir");
        let now = 1_000_000u64;
        fs::write(root.join("fresh.json"), r#"{"recorded_at":999900}"#).expect("write fresh");
        fs::write(root.join("stale.json"), r#"{"recorded_at":999000}"#).expect("write stale");
        fs::write(root.join("orphan.tmp"), b"partial").expect("write tmp");

        let summary = summarize_timed_json_cache(&root, now, |contents| {
            serde_json::from_str::<serde_json::Value>(contents)
                .ok()
                .and_then(|value| value.get("recorded_at").and_then(|v| v.as_u64()))
        })
        .expect("summary");
        assert_eq!(summary.entries, 2);
        assert_eq!(summary.oldest_age_secs, Some(1000));
        assert_eq!(summary.newest_age_secs, Some(100));

        let removed = prune_timed_json_cache(&root, now, Some(500), |contents| {
            serde_json::from_str::<serde_json::Value>(contents)
                .ok()
                .and_then(|value| value.get("recorded_at").and_then(|v| v.as_u64()))
        })
        .expect("prune");
        assert_eq!(removed, 2);
        assert!(!root.join("stale.json").exists());
        assert!(!root.join("orphan.tmp").exists());
        assert!(root.join("fresh.json").exists());
    }

    #[test]
    fn remove_entry_deletes_json_and_tmp() {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("cache");
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(root.join("abc.json"), b"{}").expect("write json");
        fs::write(root.join("abc.tmp"), b"partial").expect("write tmp");
        assert!(remove_timed_json_entry(&root, "abc").expect("remove"));
        assert!(!root.join("abc.json").exists());
        assert!(!root.join("abc.tmp").exists());
    }

    #[test]
    fn remove_entry_rejects_path_traversal_stem() {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("cache");
        let victim = temp.path().join("victim.json");
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(&victim, b"{}").expect("write victim");

        let err = remove_timed_json_entry(&root, "../victim").expect_err("reject traversal");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(victim.exists());
    }

    #[test]
    fn accepts_blake3_hex_digest_stem() {
        let digest = "a".repeat(64);
        assert!(is_valid_timed_json_entry_stem(&digest));
        let mixed = format!("{}{}", "a".repeat(63), "G");
        assert!(!is_valid_timed_json_entry_stem(&mixed));
    }
}
