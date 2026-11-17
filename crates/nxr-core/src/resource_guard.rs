//! Hold cargo output locks for the life of a writer process group.
//!
//! The guardian's exclusive file lock is the ownership. A pid file is not.
//! `Uncertain` liveness keeps the lock; only `Dead` releases it.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use fs2::FileExt;

use crate::target_lock::{self, TargetAdmission};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::unistd::Pid;

const GUARDIAN_POLL: Duration = Duration::from_millis(200);

/// Whether a writer process group still owns its output lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupLiveness {
    Alive,
    Dead,
    Uncertain,
}

/// Probe the process group, then the leader pid.
///
/// `kill(-pgid, None)` keeps the lock while any member of the group is alive,
/// including after the leader exits. `Ok` and `EPERM` are
/// [`GroupLiveness::Alive`]. `ESRCH` for both the group and the pid is
/// [`GroupLiveness::Dead`]. Pid 0, a pid that does not fit in `i32`, any other
/// error, and non-unix targets are [`GroupLiveness::Uncertain`].
///
/// When `expected_start` is `Some` and the process start time can be read, a
/// different start time is [`GroupLiveness::Dead`] (pid reuse). An unreadable
/// start time leaves the kill result unchanged.
#[must_use]
pub fn writer_group_alive(pgid: u32, expected_start: Option<u64>) -> GroupLiveness {
    #[cfg(not(unix))]
    {
        let _ = (pgid, expected_start);
        GroupLiveness::Uncertain
    }
    #[cfg(unix)]
    {
        unix_writer_group_alive(pgid, expected_start)
    }
}

/// Hold an exclusive lock on `lock_path` until `writer_pgid` is dead.
///
/// Parent directories are created. The lock is blocking. Alive and uncertain
/// probes wait 200ms and try again. A dead probe drops the lock and returns.
///
/// # Errors
///
/// Returns an error when the parent directory cannot be created, the lock file
/// cannot be opened, or the exclusive lock cannot be taken.
pub fn guardian_loop(lock_path: &Path, writer_pgid: u32) -> Result<(), String> {
    if let Some(parent) = lock_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create guardian lock parent {}: {err}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|err| format!("open guardian lock {}: {err}", lock_path.display()))?;
    file.lock_exclusive()
        .map_err(|err| format!("lock guardian {}: {err}", lock_path.display()))?;

    while writer_group_alive(writer_pgid, None) != GroupLiveness::Dead {
        thread::sleep(GUARDIAN_POLL);
    }
    let _ = file.unlock();
    Ok(())
}

/// Admit every path, lowest canonical display first.
///
/// A later failure drops the admissions already held and returns that error.
///
/// # Errors
///
/// Returns the error from `target_lock::admit_target`. The same directory
/// twice fails with `already has a running local build`.
#[must_use = "dropping releases the target directory locks"]
pub fn acquire_ordered(paths: &[PathBuf]) -> Result<Vec<TargetAdmission>, String> {
    let mut ordered: Vec<&PathBuf> = paths.iter().collect();
    ordered.sort_by_cached_key(|path| canonical_sort_key(path));

    let mut admitted = Vec::with_capacity(ordered.len());
    for path in ordered {
        match target_lock::admit_target(path) {
            Ok(admission) => admitted.push(admission),
            Err(err) => {
                drop(admitted);
                return Err(err);
            }
        }
    }
    Ok(admitted)
}

fn canonical_sort_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
        .display()
        .to_string()
}

#[cfg(unix)]
fn unix_writer_group_alive(pgid: u32, expected_start: Option<u64>) -> GroupLiveness {
    if pgid == 0 {
        return GroupLiveness::Uncertain;
    }
    let Ok(raw) = i32::try_from(pgid) else {
        return GroupLiveness::Uncertain;
    };
    let liveness = group_or_process_liveness(raw);
    if liveness != GroupLiveness::Dead && expected_start_contradicts(pgid, expected_start) {
        GroupLiveness::Dead
    } else {
        liveness
    }
}

#[cfg(unix)]
fn group_or_process_liveness(raw: i32) -> GroupLiveness {
    let group = raw
        .checked_neg()
        .map(|negative| kill(Pid::from_raw(negative), None));
    match group {
        Some(Ok(()) | Err(Errno::EPERM)) => GroupLiveness::Alive,
        Some(Err(Errno::ESRCH)) | None => match kill(Pid::from_raw(raw), None) {
            Ok(()) | Err(Errno::EPERM) => GroupLiveness::Alive,
            Err(Errno::ESRCH) => GroupLiveness::Dead,
            Err(_) => GroupLiveness::Uncertain,
        },
        Some(Err(_)) => GroupLiveness::Uncertain,
    }
}

/// Re-exec `nxr` to hold `lock_path` until `writer_pgid`'s group is dead.
///
/// A test harness is not re-executed. The guardian is a separate process, so
/// the supervisor's in-memory guard map is not the ownership record.
///
/// # Errors
///
/// Returns an error when the `nxr` binary cannot be spawned.
pub fn spawn_guardian(lock_path: &Path, writer_pgid: u32) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| format!("guardian executable: {err}"))?;
    if exe.file_name().and_then(|name| name.to_str()) != Some("nxr") {
        return Ok(());
    }
    let mut command = std::process::Command::new(exe);
    command
        .env("NXR_RESOURCE_GUARDIAN", "1")
        .env("NXR_GUARDIAN_LOCK", lock_path)
        .env("NXR_GUARDIAN_PGID", writer_pgid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("spawn guardian: {err}"))
}

#[cfg(unix)]
fn expected_start_contradicts(pid: u32, expected_start: Option<u64>) -> bool {
    let Some(expected) = expected_start else {
        return false;
    };
    match process_start_time(pid) {
        Some(actual) => actual != expected,
        None => false,
    }
}

#[cfg(all(unix, target_os = "linux"))]
fn process_start_time(pid: u32) -> Option<u64> {
    linux_start_time(pid)
}

#[cfg(all(unix, target_os = "macos"))]
fn process_start_time(pid: u32) -> Option<u64> {
    macos_start_time(pid)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_start_time(pid: u32) -> Option<u64> {
    let _ = pid;
    None
}

#[cfg(target_os = "linux")]
fn linux_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_stat_start_time(&stat)
}

/// `starttime` is field 22 of `/proc/<pid>/stat`, index 19 after `comm`.
#[cfg(any(test, target_os = "linux"))]
fn parse_linux_stat_start_time(stat: &str) -> Option<u64> {
    let end = stat.rfind(')')?;
    stat[end + 1..].split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_start_time(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stamp = String::from_utf8(output.stdout).ok()?;
    let stamp = stamp.trim();
    if stamp.is_empty() {
        None
    } else {
        Some(fnv1a64(stamp.as_bytes()))
    }
}

#[cfg(target_os = "macos")]
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_ordered_two_directories_second_admit_fails_until_drop() {
        let left = tempfile::tempdir().expect("left dir");
        let right = tempfile::tempdir().expect("right dir");
        let left_path = left.path().to_path_buf();
        let right_path = right.path().to_path_buf();

        let guards =
            acquire_ordered(&[right_path.clone(), left_path.clone()]).expect("two directories");
        assert_eq!(guards.len(), 2);

        let err = crate::target_lock::admit_target(&left_path).expect_err("left still held");
        assert!(err.contains("already has a running local build"), "{err}");
        let err = crate::target_lock::admit_target(&right_path).expect_err("right still held");
        assert!(err.contains("already has a running local build"), "{err}");

        drop(guards);
        let again = crate::target_lock::admit_target(&left_path).expect("admission after drop");
        drop(again);

        let duplicate = acquire_ordered(&[left_path.clone(), left_path.clone()])
            .expect_err("same directory twice");
        assert!(
            duplicate.contains("already has a running local build"),
            "{duplicate}"
        );
        let recovered =
            crate::target_lock::admit_target(&left_path).expect("failed acquire released");
        drop(recovered);
    }

    #[test]
    fn writer_group_alive_esrch_pid_is_dead() {
        let pid = 2_147_000_000;
        #[cfg(unix)]
        {
            use nix::errno::Errno;
            use nix::sys::signal::kill;
            use nix::unistd::Pid;

            let raw = i32::try_from(pid).expect("pid fits in i32");
            let err = kill(Pid::from_raw(raw), None).expect_err("probe pid");
            assert_eq!(err, Errno::ESRCH, "probe pid must be ESRCH");
            assert_eq!(writer_group_alive(pid, None), GroupLiveness::Dead);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(writer_group_alive(pid, None), GroupLiveness::Uncertain);
        }
    }

    #[test]
    fn writer_group_alive_current_process_is_alive() {
        let pid = std::process::id();
        #[cfg(unix)]
        assert_eq!(writer_group_alive(pid, None), GroupLiveness::Alive);
        #[cfg(not(unix))]
        assert_eq!(writer_group_alive(pid, None), GroupLiveness::Uncertain);
    }

    #[test]
    fn writer_group_alive_pid_zero_and_overflow_are_uncertain() {
        assert_eq!(writer_group_alive(0, None), GroupLiveness::Uncertain);
        assert_eq!(
            writer_group_alive(u32::MAX, Some(1)),
            GroupLiveness::Uncertain
        );
    }

    #[cfg(unix)]
    #[test]
    fn guardian_loop_returns_when_writer_is_dead() {
        use nix::errno::Errno;
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        let pid = 2_147_000_000;
        let raw = i32::try_from(pid).expect("pid fits in i32");
        let err = kill(Pid::from_raw(raw), None).expect_err("probe pid");
        assert_eq!(err, Errno::ESRCH, "probe pid must be ESRCH");
        assert_eq!(
            writer_group_alive(pid, None),
            GroupLiveness::Dead,
            "do not enter the guardian unless the writer is already dead"
        );
        let root = tempfile::tempdir().expect("temp dir");
        let lock_path = root.path().join("locks").join("writer.lock");
        guardian_loop(&lock_path, pid).expect("dead writer releases the lock");
        assert!(lock_path.is_file(), "guardian creates the lock file");
    }

    #[test]
    fn linux_start_time_is_proc_field_22() {
        // After `comm`, index 19 is starttime. Index 20 is vsize.
        let fields = [
            "S", "1", "1", "1", "0", "-1", "0", "0", "0", "0", "0", "0", "0", "0", "0", "20", "0",
            "1", "0", "999", "12345",
        ];
        assert_eq!(fields[19], "999");
        let stat = format!("123 (comm with spaces) {}", fields.join(" "));
        assert_eq!(parse_linux_stat_start_time(&stat), Some(999));
    }
}
