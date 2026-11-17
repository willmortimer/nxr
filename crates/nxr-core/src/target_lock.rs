//! Exclusive admission for one Cargo target directory.
//!
//! [`admit_target`] covers only callers that hold a [`TargetAdmission`].
//! A cargo process that never calls it is outside this lock.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use fs2::FileExt;

#[cfg(unix)]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::unistd::Pid;

const LOCK_FILE: &str = ".nxr-target.lock";
const OWNER_FILE: &str = ".nxr-target.owner";

/// Directories this process currently admits.
///
/// Each live admission holds its own lock-file descriptor. On macOS, `flock`
/// is per-process: a second `try_lock_exclusive` in this process succeeds, and
/// `unlock` or a close of that second descriptor can release the admission
/// that is still alive. The set rejects that second admission before another
/// descriptor is locked. A different process still conflicts on `flock`.
static ADMITTED: std::sync::LazyLock<Mutex<HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

/// Cargo target directory for one command.
///
/// The first `--target-dir <path>` or `--target-dir=<path>` wins. Otherwise a
/// non-empty `cargo_target_dir` is used. Otherwise the directory is
/// `<workspace_root>/target`. Relative paths are joined to `workspace_root`.
#[must_use]
pub fn effective_target_dir(
    workspace_root: &Path,
    cargo_target_dir: Option<&str>,
    command: &[String],
) -> PathBuf {
    if let Some(from_command) = command_target_dir(command) {
        return rooted(workspace_root, from_command);
    }
    if let Some(from_env) = cargo_target_dir.filter(|value| !value.is_empty()) {
        return rooted(workspace_root, from_env);
    }
    workspace_root.join("target")
}

/// Held exclusive admission for one Cargo target directory.
///
/// Drop removes this process's owner record when it is still current, then
/// unlocks the directory.
#[must_use = "dropping releases the target directory lock"]
#[derive(Debug)]
pub struct TargetAdmission {
    lock: File,
    owner_path: PathBuf,
    pid: u32,
    key: PathBuf,
}

impl Drop for TargetAdmission {
    fn drop(&mut self) {
        if let Ok(text) = fs::read_to_string(&self.owner_path)
            && parse_owner_pid(&text) == Some(self.pid)
        {
            let _ = fs::remove_file(&self.owner_path);
        }
        release_held(&self.key, &self.lock);
    }
}

/// Admit one local build against `target_dir`.
///
/// The directory is created when it is missing. The returned guard holds
/// `.nxr-target.lock` until it is dropped.
///
/// # Errors
///
/// Returns an error containing `already has a running local build` when this
/// process or another process already holds the directory. Returns an error
/// containing `target owner is uncertain` when the owner file is present but
/// is not a dead pid and is not this process. An absent owner file, or a pid
/// that `kill` reports as `ESRCH`, is replaced with this process id.
pub fn admit_target(target_dir: &Path) -> Result<TargetAdmission, String> {
    fs::create_dir_all(target_dir)
        .map_err(|err| format!("create target dir {}: {err}", target_dir.display()))?;
    let key = fs::canonicalize(target_dir).unwrap_or_else(|_| target_dir.to_path_buf());
    let pid = std::process::id();
    let owner_path = target_dir.join(OWNER_FILE);

    let lock = {
        let mut held = admitted();
        if held.contains(&key) {
            return Err(running_build(target_dir));
        }
        let lock = open_lock(target_dir)?;
        if lock.try_lock_exclusive().is_err() {
            return Err(running_build(target_dir));
        }
        held.insert(key.clone());
        lock
    };
    let permit = LockPermit {
        key,
        file: Some(lock),
        release_on_drop: true,
    };
    claim_owner(target_dir, &owner_path, pid)?;
    Ok(permit.into_admission(owner_path, pid))
}

/// Lock held after `flock` succeeds and before the owner record is claimed.
///
/// Drop releases the in-process reservation and the file lock unless the
/// permit has been turned into a [`TargetAdmission`].
struct LockPermit {
    key: PathBuf,
    file: Option<File>,
    release_on_drop: bool,
}

impl LockPermit {
    fn into_admission(mut self, owner_path: PathBuf, pid: u32) -> TargetAdmission {
        let lock = self.file.take().expect("held lock file");
        let key = mem::take(&mut self.key);
        self.release_on_drop = false;
        TargetAdmission {
            lock,
            owner_path,
            pid,
            key,
        }
    }
}

impl Drop for LockPermit {
    fn drop(&mut self) {
        if !self.release_on_drop {
            return;
        }
        if let Some(file) = self.file.as_ref() {
            release_held(&self.key, file);
        } else {
            admitted().remove(&self.key);
        }
    }
}

fn command_target_dir(command: &[String]) -> Option<&str> {
    let mut args = command.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--target-dir=")
            && !value.is_empty()
        {
            return Some(value);
        }
        if arg == "--target-dir"
            && let Some(value) = args.next()
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

fn rooted(workspace_root: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn open_lock(target_dir: &Path) -> Result<File, String> {
    let path = target_dir.join(LOCK_FILE);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| format!("open target lock {}: {err}", path.display()))
}

fn claim_owner(target_dir: &Path, owner_path: &Path, pid: u32) -> Result<(), String> {
    match fs::read_to_string(owner_path) {
        Err(err) if err.kind() == ErrorKind::NotFound => write_owner(owner_path, pid),
        Err(_) => Err(uncertain(target_dir)),
        Ok(text) => match parse_owner_pid(&text) {
            Some(owner) if owner_reusable(owner, pid) => write_owner(owner_path, pid),
            Some(_) | None => Err(uncertain(target_dir)),
        },
    }
}

/// A dead pid can be replaced. This process may refresh its own record.
/// Any other live pid, or a pid whose liveness cannot be known, stays put.
fn owner_reusable(owner: u32, pid: u32) -> bool {
    match owner_liveness(owner) {
        Liveness::Dead => true,
        Liveness::Alive => owner == pid,
        Liveness::Uncertain => false,
    }
}

fn write_owner(path: &Path, pid: u32) -> Result<(), String> {
    let temporary = path.with_file_name(".nxr-target.owner.tmp");
    fs::write(&temporary, format!("{pid}\n"))
        .map_err(|err| format!("write target owner {}: {err}", temporary.display()))?;
    if let Err(err) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("replace target owner {}: {err}", path.display()));
    }
    Ok(())
}

fn parse_owner_pid(text: &str) -> Option<u32> {
    let line = text.trim();
    if line.is_empty() {
        None
    } else {
        line.parse().ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Liveness {
    Alive,
    Dead,
    Uncertain,
}

fn owner_liveness(pid: u32) -> Liveness {
    #[cfg(unix)]
    {
        let Ok(raw) = i32::try_from(pid) else {
            return Liveness::Uncertain;
        };
        match kill(Pid::from_raw(raw), None) {
            Ok(()) | Err(nix::errno::Errno::EPERM) => Liveness::Alive,
            Err(nix::errno::Errno::ESRCH) => Liveness::Dead,
            Err(_) => Liveness::Uncertain,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Liveness::Uncertain
    }
}

fn running_build(target_dir: &Path) -> String {
    format!(
        "cargo target {} already has a running local build",
        target_dir.display()
    )
}

fn uncertain(target_dir: &Path) -> String {
    format!("target owner is uncertain for {}", target_dir.display())
}

fn admitted() -> MutexGuard<'static, HashSet<PathBuf>> {
    ADMITTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn release_held(key: &Path, lock: &File) {
    let mut held = admitted();
    held.remove(key);
    let _ = lock.unlock();
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;

    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().copied().map(str::to_owned).collect()
    }

    #[test]
    fn command_target_dir_wins_over_cargo_target_dir_and_workspace() {
        let workspace = Path::new("/repo");
        let command = argv(&["cargo", "build", "--target-dir", "/explicit", "--release"]);
        assert_eq!(
            effective_target_dir(workspace, Some("/from-env"), &command),
            PathBuf::from("/explicit")
        );
    }

    #[test]
    fn cargo_target_dir_wins_when_command_has_no_target_dir() {
        let workspace = Path::new("/repo");
        let command = argv(&["cargo", "build", "--release"]);
        assert_eq!(
            effective_target_dir(workspace, Some("/from-env"), &command),
            PathBuf::from("/from-env")
        );
        assert_eq!(
            effective_target_dir(workspace, Some("env-relative"), &command),
            PathBuf::from("/repo/env-relative")
        );
    }

    #[test]
    fn workspace_target_is_the_fallback() {
        let workspace = Path::new("/repo");
        let command = argv(&["cargo", "check"]);
        assert_eq!(
            effective_target_dir(workspace, None, &command),
            workspace.join("target")
        );
        assert_eq!(
            effective_target_dir(workspace, Some(""), &command),
            workspace.join("target")
        );
    }

    #[test]
    fn relative_target_dir_joins_the_workspace() {
        let workspace = Path::new("/repo");
        let command = argv(&["cargo", "build", "--target-dir=custom"]);
        assert_eq!(
            effective_target_dir(workspace, Some("/from-env"), &command),
            workspace.join("custom")
        );
    }

    #[test]
    fn second_admission_of_one_directory_fails_until_drop() {
        let root = tempfile::tempdir().expect("temp dir");
        let first = admit_target(root.path()).expect("first admission");
        let err = admit_target(root.path()).expect_err("second admission");
        assert!(err.contains("already has a running local build"), "{err}");
        let owner = fs::read_to_string(root.path().join(OWNER_FILE)).expect("owner");
        assert_eq!(owner.trim(), process::id().to_string());
        drop(first);
        assert!(!root.path().join(OWNER_FILE).exists());
        let third = admit_target(root.path()).expect("admission after drop");
        drop(third);
    }

    #[test]
    fn two_directories_both_succeed() {
        let left = tempfile::tempdir().expect("left dir");
        let right = tempfile::tempdir().expect("right dir");
        let admit_left = admit_target(left.path()).expect("left admission");
        let admit_right = admit_target(right.path()).expect("right admission");
        assert_eq!(
            fs::read_to_string(left.path().join(OWNER_FILE))
                .expect("left owner")
                .trim(),
            process::id().to_string()
        );
        assert_eq!(
            fs::read_to_string(right.path().join(OWNER_FILE))
                .expect("right owner")
                .trim(),
            process::id().to_string()
        );
        drop(admit_left);
        drop(admit_right);
    }

    #[test]
    fn dead_owner_pid_is_replaced() {
        let dead = 2_147_000_000;
        assert_eq!(
            owner_liveness(dead),
            Liveness::Dead,
            "probe pid must be ESRCH"
        );
        let root = tempfile::tempdir().expect("temp dir");
        let owner = root.path().join(OWNER_FILE);
        fs::write(&owner, format!("{dead}\n")).expect("write dead owner");
        let admission = admit_target(root.path()).expect("dead owner is recoverable");
        let text = fs::read_to_string(&owner).expect("owner replaced");
        assert_eq!(text.trim(), process::id().to_string());
        drop(admission);
    }

    #[test]
    fn malformed_owner_is_uncertain_and_preserved() {
        let root = tempfile::tempdir().expect("temp dir");
        let owner = root.path().join(OWNER_FILE);
        fs::write(&owner, "not-a-pid").expect("write owner");
        let err = admit_target(root.path()).expect_err("malformed owner");
        assert!(err.contains("target owner is uncertain"), "{err}");
        assert_eq!(
            fs::read_to_string(&owner).expect("owner preserved"),
            "not-a-pid"
        );
        let again = admit_target(root.path()).expect_err("lock released after uncertain owner");
        assert!(again.contains("target owner is uncertain"), "{again}");
        assert_eq!(
            fs::read_to_string(&owner).expect("owner still preserved"),
            "not-a-pid"
        );
    }
}
