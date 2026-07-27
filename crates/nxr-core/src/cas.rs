//! Local content-addressable store for workspace action outputs.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use camino::Utf8Path;
use serde::{Deserialize, Serialize};

/// NXR workspace CAS protocol version (bump when entry layout or key material changes).
pub const CAS_PROTOCOL_VERSION: u32 = 1;

/// Environment variable disabling workspace CAS (`off`, `0`, or `false`).
pub const WORKSPACE_CAS_ENV: &str = "NXR_WORKSPACE_CAS";

/// Result of looking up a workspace action in the local CAS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CasLookup {
    /// Entry exists and outputs were restored.
    Hit,
    /// No entry or restore disabled.
    Miss { reason: String },
}

/// Human-readable cache decision for explain / dry-run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CacheExplain {
    pub tier: super::action::ActionTier,
    pub cache_enabled: bool,
    pub action_key: Option<String>,
    pub lookup: CacheLookupExplain,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub key_components: BTreeMap<String, String>,
}

/// Serializable lookup outcome for explain output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum CacheLookupExplain {
    Hit,
    Miss { reason: String },
    Skipped { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CasManifest {
    protocol_version: u32,
    action_key: String,
    outputs: BTreeMap<String, String>,
}

/// On-disk workspace CAS summary for `nxr cache status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceCasStatus {
    pub path: String,
    pub entries: usize,
    pub total_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_CAS_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Whether workspace CAS restore/save is enabled.
#[must_use]
pub fn workspace_cas_enabled() -> bool {
    workspace_cas_enabled_for_env(std::env::var(WORKSPACE_CAS_ENV).ok().as_deref())
}

fn workspace_cas_enabled_for_env(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Workspace CAS root under the XDG cache dir (`…/nxr/cas`).
#[must_use]
pub fn workspace_cas_dir() -> Option<PathBuf> {
    cas_root().map(|root| root.join("entries"))
}

fn cas_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_CAS_ROOT.with(|cell| cell.borrow().clone()) {
        return Some(root);
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr").map(|dirs| dirs.cache_dir().join("cas"))
}

/// Hex-encoded BLAKE3 digest of canonical `key_material` JSON.
#[must_use]
pub fn hash_action_key(key_material: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(key_material).unwrap_or_default();
    let hash = blake3::hash(canonical.as_bytes());
    hash.to_hex().to_string()
}

/// Digest a single file's contents (hex BLAKE3).
///
/// # Errors
///
/// Returns [`io::Error`] when the path cannot be read.
pub fn digest_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 65_536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Digest a repo-relative path under `flake_root` (file or directory tree).
///
/// Directories are walked in sorted path order; missing paths hash as empty.
///
/// # Errors
///
/// Returns [`io::Error`] when traversal or reading fails.
pub fn digest_repo_path(flake_root: &Utf8Path, relative: &str) -> io::Result<String> {
    let path = flake_root.join(relative);
    if !path.exists() {
        return Ok(blake3::hash(b"missing").to_hex().to_string());
    }
    if path.is_file() {
        return digest_file(path.as_std_path());
    }
    let mut hasher = Hasher::new();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(path.as_std_path(), &mut files)?;
    files.sort();
    for file in files {
        let rel = file
            .strip_prefix(path.as_std_path())
            .unwrap_or(&file)
            .to_string_lossy();
        hasher.update(rel.as_bytes());
        hasher.update(digest_file(&file)?.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Digest `flake.lock` when present at the flake root.
///
/// # Errors
///
/// Returns [`io::Error`] when the lockfile cannot be read.
pub fn flake_lock_digest(flake_root: &Utf8Path) -> io::Result<Option<String>> {
    let lock = flake_root.join("flake.lock");
    if !lock.is_file() {
        return Ok(None);
    }
    Ok(Some(digest_file(lock.as_std_path())?))
}

fn entry_dir(action_key: &str) -> Option<PathBuf> {
    workspace_cas_dir().map(|root| root.join(action_key))
}

/// Look up a workspace action in the local CAS without restoring files.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache cannot be read.
pub fn lookup_outputs(action_key: &str, output_paths: &[String]) -> io::Result<CasLookup> {
    if !workspace_cas_enabled() {
        return Ok(CasLookup::Miss {
            reason: "workspace CAS disabled via NXR_WORKSPACE_CAS".to_owned(),
        });
    }
    let Some(entry) = entry_dir(action_key) else {
        return Ok(CasLookup::Miss {
            reason: "workspace CAS directory unavailable".to_owned(),
        });
    };
    let manifest_path = entry.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(CasLookup::Miss {
            reason: "no CAS entry for action key".to_owned(),
        });
    }
    let manifest: CasManifest = serde_json::from_reader(File::open(&manifest_path)?)?;
    if manifest.action_key != action_key {
        return Ok(CasLookup::Miss {
            reason: "manifest action key mismatch".to_owned(),
        });
    }
    let data_dir = entry.join("data");
    let data_utf8 = Utf8Path::from_path(&data_dir).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace CAS data path is not UTF-8",
        )
    })?;
    for relative in output_paths {
        let Some(expected_digest) = manifest.outputs.get(relative) else {
            return Ok(CasLookup::Miss {
                reason: format!("manifest missing output path `{relative}`"),
            });
        };
        let source = data_dir.join(relative);
        if !source.exists() {
            return Ok(CasLookup::Miss {
                reason: format!("CAS data missing for `{relative}`"),
            });
        }
        let actual = digest_repo_path(data_utf8, relative)?;
        if &actual != expected_digest {
            return Ok(CasLookup::Miss {
                reason: format!("digest mismatch for `{relative}`"),
            });
        }
    }
    Ok(CasLookup::Hit)
}

/// Restore declared outputs from the local CAS when an entry exists.
///
/// # Errors
///
/// Returns [`io::Error`] when restore I/O fails.
pub fn restore_outputs(
    flake_root: &Utf8Path,
    action_key: &str,
    output_paths: &[String],
) -> io::Result<CasLookup> {
    match lookup_outputs(action_key, output_paths)? {
        CasLookup::Miss { reason } => Ok(CasLookup::Miss { reason }),
        CasLookup::Hit => {
            let Some(entry) = entry_dir(action_key) else {
                return Ok(CasLookup::Miss {
                    reason: "workspace CAS directory unavailable".to_owned(),
                });
            };
            let data_dir = entry.join("data");
            for relative in output_paths {
                let source = data_dir.join(relative);
                let dest = flake_root.join(relative);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_tree(&source, dest.as_std_path())?;
            }
            Ok(CasLookup::Hit)
        }
    }
}

/// Persist workspace outputs into the local CAS after a successful run.
///
/// # Errors
///
/// Returns [`io::Error`] when save I/O fails.
pub fn save_outputs(
    flake_root: &Utf8Path,
    action_key: &str,
    output_paths: &[String],
) -> io::Result<()> {
    if !workspace_cas_enabled() {
        return Ok(());
    }
    let Some(entry) = entry_dir(action_key) else {
        return Ok(());
    };
    if entry.exists() {
        fs::remove_dir_all(&entry)?;
    }
    fs::create_dir_all(entry.join("data"))?;
    let mut outputs = BTreeMap::new();
    for relative in output_paths {
        let source = flake_root.join(relative);
        if !source.exists() {
            continue;
        }
        outputs.insert(relative.clone(), digest_repo_path(flake_root, relative)?);
        let dest = entry.join("data").join(relative);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_tree(source.as_std_path(), &dest)?;
    }
    let manifest = CasManifest {
        protocol_version: CAS_PROTOCOL_VERSION,
        action_key: action_key.to_owned(),
        outputs,
    };
    let manifest_path = entry.join("manifest.json");
    let mut file = File::create(manifest_path)?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to)?;
        return Ok(());
    }
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Remove all workspace CAS entries.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read or removed.
pub fn clear_workspace_cas() -> io::Result<usize> {
    let Some(root) = workspace_cas_dir() else {
        return Ok(0);
    };
    if !root.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Summarize workspace CAS usage for `nxr cache status`.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache directory cannot be read.
pub fn workspace_cas_status() -> io::Result<WorkspaceCasStatus> {
    let Some(root) = workspace_cas_dir() else {
        return Ok(WorkspaceCasStatus {
            path: String::new(),
            entries: 0,
            total_bytes: 0,
        });
    };
    if !root.is_dir() {
        return Ok(WorkspaceCasStatus {
            path: root.display().to_string(),
            entries: 0,
            total_bytes: 0,
        });
    }
    let mut entries = 0usize;
    let mut total_bytes = 0u64;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            entries += 1;
            total_bytes = total_bytes.saturating_add(dir_size(entry.path())?);
        }
    }
    Ok(WorkspaceCasStatus {
        path: root.display().to_string(),
        entries,
        total_bytes,
    })
}

fn dir_size(path: PathBuf) -> io::Result<u64> {
    let mut total = 0u64;
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    if !path.is_dir() {
        return Ok(0);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total = total.saturating_add(dir_size(entry.path())?);
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

#[cfg(test)]
pub(crate) fn set_test_cas_root(root: PathBuf) {
    TEST_CAS_ROOT.with(|cell| *cell.borrow_mut() = Some(root));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::tempdir;

    #[test]
    fn save_and_restore_round_trip() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let out = flake.join("out.txt");
        fs::write(&out, b"hello").expect("write output");

        let key = "abc123";
        save_outputs(&flake, key, &[String::from("out.txt")]).expect("save");
        fs::write(&out, b"stale").expect("stale output");

        let lookup = restore_outputs(&flake, key, &[String::from("out.txt")]).expect("restore");
        assert_eq!(lookup, CasLookup::Hit);
        assert_eq!(fs::read_to_string(out).expect("read"), "hello");
    }

    #[test]
    fn miss_when_entry_absent() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let lookup =
            restore_outputs(&flake, "missing", &[String::from("out.txt")]).expect("restore");
        assert!(matches!(lookup, CasLookup::Miss { .. }));
    }
}
