//! Local content-addressable store for workspace action outputs.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use blake3::Hasher;
use camino::Utf8Path;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// NXR workspace CAS protocol version (bump when entry layout or key material changes).
pub const CAS_PROTOCOL_VERSION: u32 = 2;

/// Environment variable disabling workspace CAS (`off`, `0`, or `false`).
pub const WORKSPACE_CAS_ENV: &str = "NXR_WORKSPACE_CAS";

const MISSING_PATH_MARKER: &[u8] = b"missing";

fn missing_path_digest() -> String {
    digest_bytes(MISSING_PATH_MARKER)
}

/// How a cached workspace artifact is restored from CAS.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CasRestoreMode {
    /// Remove the destination tree before copying cached data.
    Replace,
    /// Overlay cached data without deleting extra files in the destination.
    #[default]
    Merge,
    /// Validate digests only; do not write files.
    VerifyOnly,
    /// Report digest comparison only; do not write files.
    Report,
}

/// Declared workspace output for CAS save/restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CasOutput {
    pub path: String,
    pub mode: CasRestoreMode,
    pub optional: bool,
}

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

fn cas_tmp_dir() -> Option<PathBuf> {
    cas_root().map(|root| root.join("tmp"))
}

/// Hex-encoded BLAKE3 digest of canonical `key_material` JSON.
#[must_use]
pub fn hash_action_key(key_material: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(key_material).unwrap_or_default();
    digest_bytes(canonical.as_bytes())
}

/// Hex-encoded BLAKE3 digest of arbitrary bytes.
#[must_use]
pub fn digest_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// Digest a single file's contents (hex BLAKE3).
///
/// # Errors
///
/// Returns [`io::Error`] when the path cannot be read.
pub fn digest_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; 65_536];
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
/// Returns [`io::Error`] when traversal or reading fails, including symlink escape.
pub fn digest_repo_path(flake_root: &Utf8Path, relative: &str) -> io::Result<String> {
    let root = flake_root.as_std_path();
    let path = root.join(relative);
    if !path.exists() {
        return Ok(missing_path_digest());
    }
    ensure_within_workspace(root, &path)?;
    if path.is_file() {
        return digest_file(&path);
    }
    let mut hasher = Hasher::new();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, &path, &mut files)?;
    files.sort();
    for file in files {
        let rel = file
            .strip_prefix(&path)
            .unwrap_or(&file)
            .to_string_lossy();
        hasher.update(rel.as_bytes());
        hasher.update(digest_file(&file)?.as_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn collect_files(workspace_root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        let resolved = resolve_within_workspace(workspace_root, dir)?;
        if resolved.is_file() {
            out.push(resolved);
            return Ok(());
        }
        if resolved.is_dir() {
            return collect_files(workspace_root, &resolved, out);
        }
        return Ok(());
    }
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
        let entry_meta = entry.metadata()?;
        if entry_meta.file_type().is_symlink() {
            let resolved = resolve_within_workspace(workspace_root, &path)?;
            if resolved.is_dir() {
                collect_files(workspace_root, &resolved, out)?;
            } else if resolved.is_file() {
                out.push(resolved);
            }
        } else if path.is_dir() {
            collect_files(workspace_root, &path, out)?;
        } else {
            ensure_within_workspace(workspace_root, &path)?;
            out.push(path);
        }
    }
    Ok(())
}

fn ensure_within_workspace(workspace_root: &Path, path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        resolve_within_workspace(workspace_root, path).map(|_| ())
    } else {
        ensure_logical_within_workspace(workspace_root, path)
    }
}

fn ensure_logical_within_workspace(workspace_root: &Path, path: &Path) -> io::Result<()> {
    let canonical_root = workspace_root.canonicalize().map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace root `{}` cannot be canonicalized: {source}",
                workspace_root.display()
            ),
        )
    })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    if let Ok(canonical) = absolute.canonicalize() {
        if !canonical.starts_with(&canonical_root) {
            return Err(workspace_escape_error(workspace_root, path));
        }
        return Ok(());
    }
    let relative = absolute
        .strip_prefix(workspace_root)
        .unwrap_or(absolute.as_path());
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(workspace_escape_error(workspace_root, path));
    }
    Ok(())
}

fn workspace_escape_error(workspace_root: &Path, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "path `{}` escapes workspace root `{}`",
            path.display(),
            workspace_root.display()
        ),
    )
}

fn resolve_within_workspace(workspace_root: &Path, path: &Path) -> io::Result<PathBuf> {
    let canonical_root = workspace_root.canonicalize().map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace root `{}` cannot be canonicalized: {source}",
                workspace_root.display()
            ),
        )
    })?;
    let resolved = if fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        let target = fs::read_link(path)?;
        if target.is_absolute() {
            target
        } else {
            path.parent()
                .unwrap_or(workspace_root)
                .join(target)
        }
    } else {
        path.to_path_buf()
    };
    let canonical = resolved.canonicalize().map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "path `{}` resolves outside workspace or is unreadable: {source}",
                path.display()
            ),
        )
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(workspace_escape_error(workspace_root, path));
    }
    Ok(canonical)
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

fn read_manifest(action_key: &str) -> io::Result<Option<CasManifest>> {
    let Some(entry) = entry_dir(action_key) else {
        return Ok(None);
    };
    let manifest_path = entry.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest: CasManifest = serde_json::from_reader(File::open(&manifest_path)?)?;
    Ok(Some(manifest))
}

/// Look up a workspace action in the local CAS without restoring files.
///
/// # Errors
///
/// Returns [`io::Error`] when the cache cannot be read.
pub fn lookup_outputs(action_key: &str, outputs: &[CasOutput]) -> io::Result<CasLookup> {
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
    if manifest.protocol_version != CAS_PROTOCOL_VERSION {
        return Ok(CasLookup::Miss {
            reason: format!(
                "CAS protocol version mismatch (entry={}, current={})",
                manifest.protocol_version, CAS_PROTOCOL_VERSION
            ),
        });
    }
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
    for output in outputs {
        let relative = &output.path;
        let Some(expected_digest) = manifest.outputs.get(relative) else {
            if output.optional {
                continue;
            }
            return Ok(CasLookup::Miss {
                reason: format!("manifest missing output path `{relative}`"),
            });
        };
        if expected_digest == &missing_path_digest() {
            if output.optional {
                continue;
            }
            return Ok(CasLookup::Miss {
                reason: format!("required output `{relative}` was missing when cached"),
            });
        }
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
    outputs: &[CasOutput],
) -> io::Result<CasLookup> {
    match lookup_outputs(action_key, outputs)? {
        CasLookup::Miss { reason } => Ok(CasLookup::Miss { reason }),
        CasLookup::Hit => {
            let writes_outputs = outputs.iter().any(|output| {
                !matches!(
                    output.mode,
                    CasRestoreMode::VerifyOnly | CasRestoreMode::Report
                )
            });
            if !writes_outputs {
                return Ok(CasLookup::Hit);
            }
            let Some(entry) = entry_dir(action_key) else {
                return Ok(CasLookup::Miss {
                    reason: "workspace CAS directory unavailable".to_owned(),
                });
            };
            let data_dir = entry.join("data");
            let manifest = read_manifest(action_key)?
                .ok_or_else(|| io::Error::other("CAS manifest disappeared during restore"))?;
            for output in outputs {
                if matches!(
                    output.mode,
                    CasRestoreMode::VerifyOnly | CasRestoreMode::Report
                ) {
                    continue;
                }
                let relative = &output.path;
                let Some(expected_digest) = manifest.outputs.get(relative) else {
                    if output.optional {
                        continue;
                    }
                    return Ok(CasLookup::Miss {
                        reason: format!("manifest missing output path `{relative}`"),
                    });
                };
                if expected_digest == &missing_path_digest() {
                    continue;
                }
                let source = data_dir.join(relative);
                if !source.exists() {
                    continue;
                }
                let dest = flake_root.join(relative);
                if matches!(output.mode, CasRestoreMode::Replace) {
                    clear_workspace_path(flake_root, relative)?;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent.as_std_path())?;
                }
                copy_into_workspace(flake_root.as_std_path(), &source, dest.as_std_path())?;
            }
            Ok(CasLookup::Hit)
        }
    }
}

fn clear_workspace_path(flake_root: &Utf8Path, relative: &str) -> io::Result<()> {
    let dest = flake_root.join(relative);
    if !dest.exists() {
        return Ok(());
    }
    ensure_within_workspace(flake_root.as_std_path(), dest.as_std_path())?;
    if dest.is_dir() {
        fs::remove_dir_all(dest.as_std_path())?;
    } else {
        fs::remove_file(dest.as_std_path())?;
    }
    Ok(())
}

/// Persist workspace outputs into the local CAS after a successful run.
///
/// # Errors
///
/// Returns [`io::Error`] when save I/O fails.
pub fn save_outputs(
    flake_root: &Utf8Path,
    action_key: &str,
    outputs: &[CasOutput],
) -> io::Result<()> {
    if !workspace_cas_enabled() {
        return Ok(());
    }
    let Some(tmp_root) = cas_tmp_dir() else {
        return Ok(());
    };
    fs::create_dir_all(&tmp_root)?;
    let stage = tmp_root.join(stage_dir_name(action_key));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(stage.join("data"))?;
    let mut manifest_outputs = BTreeMap::new();
    for output in outputs {
        let relative = &output.path;
        let source = flake_root.join(relative);
        if !source.exists() {
            if output.optional {
                continue;
            }
            manifest_outputs.insert(relative.clone(), digest_repo_path(flake_root, relative)?);
            continue;
        }
        manifest_outputs.insert(relative.clone(), digest_repo_path(flake_root, relative)?);
        let dest = stage.join("data").join(relative);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_from_workspace(flake_root.as_std_path(), source.as_std_path(), &dest)?;
    }
    let manifest = CasManifest {
        protocol_version: CAS_PROTOCOL_VERSION,
        action_key: action_key.to_owned(),
        outputs: manifest_outputs,
    };
    let manifest_path = stage.join("manifest.json");
    write_manifest(&manifest_path, &manifest)?;
    publish_entry(action_key, &stage)
}

fn stage_dir_name(action_key: &str) -> String {
    format!("{action_key}.{}.{}", std::process::id(), stage_nonce())
}

fn stage_nonce() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64)
}

fn write_manifest(path: &Path, manifest: &CasManifest) -> io::Result<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn publish_entry(action_key: &str, stage: &Path) -> io::Result<()> {
    let Some(root) = cas_root() else {
        return Ok(());
    };
    let locks_dir = root.join("locks");
    let entries_dir = root.join("entries");
    let tmp_dir = root.join("tmp");
    fs::create_dir_all(&locks_dir)?;
    fs::create_dir_all(&entries_dir)?;
    fs::create_dir_all(&tmp_dir)?;

    let lock_path = locks_dir.join(format!("{action_key}.lock"));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let entry_path = entries_dir.join(action_key);
    let trash_path = tmp_dir.join(format!(
        "{action_key}.trash.{}.{}",
        std::process::id(),
        stage_nonce()
    ));

    let publish_result = (|| {
        if entry_path.exists() {
            fs::rename(&entry_path, &trash_path)?;
        }
        fs::rename(stage, &entry_path)?;
        sync_dir(&entry_path)?;
        if trash_path.exists() {
            fs::remove_dir_all(&trash_path)?;
        }
        Ok(())
    })();

    let _ = lock_file.unlock();
    if publish_result.is_err() && stage.exists() {
        let _ = fs::remove_dir_all(stage);
    }
    publish_result
}

fn sync_dir(path: &Path) -> io::Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()
}

fn copy_from_workspace(workspace_root: &Path, from: &Path, to: &Path) -> io::Result<()> {
    ensure_within_workspace(workspace_root, from)?;
    copy_tree_raw(from, to)
}

fn copy_into_workspace(workspace_root: &Path, from: &Path, to: &Path) -> io::Result<()> {
    ensure_within_workspace(workspace_root, to)?;
    copy_tree_raw(from, to)
}

fn copy_tree_raw(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to copy symlink `{}` in workspace CAS",
                from.display()
            ),
        ));
    }
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
        copy_tree_raw(&src, &dst)?;
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

    fn cas_output(path: &str) -> CasOutput {
        CasOutput {
            path: path.to_owned(),
            mode: CasRestoreMode::Merge,
            optional: false,
        }
    }

    fn optional_output(path: &str) -> CasOutput {
        CasOutput {
            path: path.to_owned(),
            mode: CasRestoreMode::Merge,
            optional: true,
        }
    }

    fn replace_output(path: &str) -> CasOutput {
        CasOutput {
            path: path.to_owned(),
            mode: CasRestoreMode::Replace,
            optional: false,
        }
    }

    #[test]
    fn save_and_restore_round_trip() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let out = flake.join("out.txt");
        fs::write(&out, b"hello").expect("write output");

        let key = "abc123";
        save_outputs(&flake, key, &[cas_output("out.txt")]).expect("save");
        fs::write(&out, b"stale").expect("stale output");

        let lookup = restore_outputs(&flake, key, &[cas_output("out.txt")]).expect("restore");
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
            restore_outputs(&flake, "missing", &[cas_output("out.txt")]).expect("restore");
        assert!(matches!(lookup, CasLookup::Miss { .. }));
    }

    #[test]
    fn protocol_mismatch_is_miss() {
        let tmp = tempdir().expect("tempdir");
        let cas_root = tmp.path().join("cas");
        set_test_cas_root(cas_root.clone());
        let entry = cas_root.join("entries").join("old-key");
        fs::create_dir_all(entry.join("data")).expect("entry");
        let manifest = CasManifest {
            protocol_version: 1,
            action_key: "old-key".to_owned(),
            outputs: BTreeMap::from([("out.txt".to_owned(), "deadbeef".to_owned())]),
        };
        write_manifest(&entry.join("manifest.json"), &manifest).expect("manifest");
        let lookup = lookup_outputs("old-key", &[cas_output("out.txt")]).expect("lookup");
        assert!(matches!(
            lookup,
            CasLookup::Miss {
                reason
            } if reason.contains("protocol version mismatch")
        ));
    }

    #[test]
    fn optional_missing_does_not_force_miss() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let out = flake.join("out.txt");
        fs::write(&out, b"hello").expect("write output");

        let key = "optional-key";
        save_outputs(
            &flake,
            key,
            &[cas_output("out.txt"), optional_output("extra.txt")],
        )
        .expect("save");

        let lookup = lookup_outputs(
            key,
            &[cas_output("out.txt"), optional_output("extra.txt")],
        )
        .expect("lookup");
        assert_eq!(lookup, CasLookup::Hit);
    }

    #[test]
    fn replace_mode_clears_stale_files() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        let out_dir = flake.join("out");
        fs::create_dir_all(&out_dir).expect("out dir");
        fs::write(out_dir.join("keep.txt"), b"cached").expect("cached");

        let key = "replace-key";
        save_outputs(&flake, key, &[replace_output("out")]).expect("save");

        fs::write(out_dir.join("stale.txt"), b"still-here").expect("stale");
        fs::write(out_dir.join("new-stale.txt"), b"also-remove").expect("new stale");

        let lookup = restore_outputs(&flake, key, &[replace_output("out")]).expect("restore");
        assert_eq!(lookup, CasLookup::Hit);
        assert_eq!(
            fs::read_to_string(out_dir.join("keep.txt")).expect("keep"),
            "cached"
        );
        assert!(!out_dir.join("stale.txt").exists());
        assert!(!out_dir.join("new-stale.txt").exists());
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let tmp = tempdir().expect("tempdir");
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"secret").expect("outside");
        std::os::unix::fs::symlink(&outside, flake.join("escape").as_std_path()).expect("symlink");

        let error = digest_repo_path(&flake, "escape").expect_err("digest");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("escapes workspace root"));
    }

    #[test]
    fn atomic_publish_leaves_complete_entry() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        fs::write(flake.join("out.txt"), b"hello").expect("write output");

        let key = "atomic-key";
        save_outputs(&flake, key, &[cas_output("out.txt")]).expect("save");

        let entry = tmp.path().join("cas").join("entries").join(key);
        assert!(entry.join("manifest.json").is_file());
        assert!(entry.join("data").join("out.txt").is_file());
        let manifest: CasManifest =
            serde_json::from_reader(File::open(entry.join("manifest.json")).expect("open"))
                .expect("manifest");
        assert_eq!(manifest.protocol_version, CAS_PROTOCOL_VERSION);
        assert_eq!(manifest.action_key, key);
    }

    #[test]
    fn verify_only_does_not_write_files() {
        let tmp = tempdir().expect("tempdir");
        set_test_cas_root(tmp.path().join("cas"));
        let flake = Utf8PathBuf::from_path_buf(tmp.path().join("flake")).expect("utf8");
        fs::create_dir_all(&flake).expect("flake root");
        let out = flake.join("out.txt");
        fs::write(&out, b"hello").expect("write output");

        let key = "verify-key";
        save_outputs(&flake, key, &[cas_output("out.txt")]).expect("save");
        fs::write(&out, b"stale").expect("stale output");

        let verify_output = CasOutput {
            path: "out.txt".to_owned(),
            mode: CasRestoreMode::VerifyOnly,
            optional: false,
        };
        let lookup = restore_outputs(&flake, key, &[verify_output]).expect("restore");
        assert_eq!(lookup, CasLookup::Hit);
        assert_eq!(fs::read_to_string(out).expect("read"), "stale");
    }
}
