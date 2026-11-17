//! Hold cargo output locks for the life of a writer process group.
//!
//! The guardian's exclusive file lock is the ownership. A pid file is not.
//! `Uncertain` liveness keeps the lock; only `Dead` releases it.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

fn guardian_cancel_flag() -> std::sync::Arc<AtomicBool> {
    use std::sync::OnceLock;
    static FLAG: OnceLock<std::sync::Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| {
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        {
            let _ = signal_hook::flag::register(
                signal_hook::consts::SIGTERM,
                std::sync::Arc::clone(&flag),
            );
            let _ = signal_hook::flag::register(
                signal_hook::consts::SIGINT,
                std::sync::Arc::clone(&flag),
            );
        }
        flag
    })
    .clone()
}

/// How the caller wants the writer’s standard streams wired.
#[derive(Clone, Debug)]
pub enum WriterStdio {
    /// Writer output goes to this file.
    Log(PathBuf),
    /// Writer inherits the guardian’s stdout and stderr.
    Inherit,
    /// Writer output is the guardian process pipes.
    Piped,
}

/// A command the guardian starts only after the output locks are held.
#[derive(Clone, Debug)]
pub struct WriterRequest {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub output_dirs: Vec<PathBuf>,
    pub stdio: WriterStdio,
    pub probe_out: Option<PathBuf>,
    pub probe_script: Option<String>,
    /// When set, spawn this binary as the guardian instead of the current exe.
    pub guardian_bin: Option<PathBuf>,
    /// Created after the locks are held and removed by the caller before spawn.
    pub pause_file: Option<PathBuf>,
    /// Drop the inherited environment and keep only `env`.
    pub clear_env: bool,
}

/// A writer that is running, and the process that owns its output locks.
#[derive(Debug)]
pub struct LaunchedWriter {
    pub writer_pid: u32,
    pub guardian: Option<Child>,
    pub child: Option<Child>,
    pub admissions: Vec<TargetAdmission>,
    pub status_path: Option<PathBuf>,
}

/// Acquire `output_dirs`, then start `argv`.
///
/// A real `nxr` guardian is used when `guardian_bin` is set or this process is
/// `nxr`. Otherwise the locks are held in this process. Failure returns before
/// a writer exists.
///
/// # Errors
///
/// Returns an error when the locks cannot be taken, the guardian cannot be
/// spawned, or the guardian exits without a ready writer.
pub fn launch_writer(request: &WriterRequest) -> Result<LaunchedWriter, String> {
    if request.argv.is_empty() {
        return Err("writer command is empty".to_owned());
    }
    if request.output_dirs.is_empty() {
        let child = spawn_direct(request, false)?;
        let writer_pid = child.id();
        return Ok(LaunchedWriter {
            writer_pid,
            guardian: None,
            child: Some(child),
            admissions: Vec::new(),
            status_path: None,
        });
    }
    if let Some(bin) = guardian_bin(request) {
        return launch_guardian_process(request, &bin);
    }
    let admissions = acquire_ordered(&request.output_dirs)?;
    let child = match spawn_direct(request, true) {
        Ok(child) => child,
        Err(err) => {
            drop(admissions);
            return Err(err);
        }
    };
    let writer_pid = child.id();
    Ok(LaunchedWriter {
        writer_pid,
        guardian: None,
        child: Some(child),
        admissions,
        status_path: None,
    })
}

fn guardian_bin(request: &WriterRequest) -> Option<PathBuf> {
    if let Some(path) = &request.guardian_bin {
        return Some(path.clone());
    }
    let Ok(exe) = std::env::current_exe() else {
        return None;
    };
    if exe.file_name().and_then(|name| name.to_str()) == Some("nxr") {
        Some(exe)
    } else {
        None
    }
}

fn launch_guardian_process(request: &WriterRequest, bin: &Path) -> Result<LaunchedWriter, String> {
    let status_path = std::env::temp_dir().join(format!(
        "nxr-guardian-{}-{}.status",
        std::process::id(),
        unique_suffix()
    ));
    let probe_file = write_probe_file(request.probe_script.as_deref())?;
    let mut command = Command::new(bin);
    command
        .env("NXR_RESOURCE_GUARDIAN", "1")
        .env("NXR_GUARDIAN_DIRS", join_dirs(&request.output_dirs))
        .env(
            "NXR_GUARDIAN_ARGV",
            serde_json::to_string(&request.argv).unwrap_or_default(),
        )
        .env("NXR_GUARDIAN_STATUS", &status_path)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = &request.cwd {
        command.env("NXR_GUARDIAN_CWD", cwd);
    }
    if let Some(probe) = &request.probe_out {
        command.env("NXR_GUARDIAN_PROBE_OUT", probe);
    }
    if let Some(path) = &probe_file {
        command.env("NXR_GUARDIAN_PROBE_FILE", path);
    }
    if let Some(pause) = &request.pause_file {
        command.env("NXR_GUARDIAN_PAUSE", pause);
    }
    if request.clear_env {
        command.env("NXR_GUARDIAN_CLEAR", "1");
    }
    if !request.env.is_empty() {
        command.env(
            "NXR_GUARDIAN_ENV",
            serde_json::to_string(&request.env).unwrap_or_else(|_| "[]".to_owned()),
        );
    }
    match &request.stdio {
        WriterStdio::Log(path) => {
            command.env("NXR_GUARDIAN_LOG", path);
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
        }
        WriterStdio::Inherit => {
            command.stdout(Stdio::inherit());
            command.stderr(Stdio::inherit());
        }
        WriterStdio::Piped => {
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        }
    }
    for (key, value) in &request.env {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut guardian = command
        .spawn()
        .map_err(|err| format!("spawn guardian: {err}"))?;
    match wait_ready(&mut guardian, &status_path) {
        Ok(writer_pid) => Ok(LaunchedWriter {
            writer_pid,
            guardian: Some(guardian),
            child: None,
            admissions: Vec::new(),
            status_path: Some(status_path),
        }),
        Err(err) => {
            let _ = guardian.kill();
            let _ = guardian.wait();
            let _ = fs::remove_file(&status_path);
            Err(err)
        }
    }
}

fn wait_ready(guardian: &mut Child, status_path: &Path) -> Result<u32, String> {
    for _ in 0..600 {
        if let Ok(text) = fs::read_to_string(status_path) {
            if let Some(rest) = text.strip_prefix("ready ") {
                let pid: u32 = rest
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .parse()
                    .map_err(|_| format!("guardian ready line is not a pid: {text}"))?;
                return Ok(pid);
            }
            if let Some(rest) = text.strip_prefix("error ") {
                return Err(rest.lines().next().unwrap_or(rest).trim().to_owned());
            }
        }
        if guardian.try_wait().ok().flatten().is_some() {
            let text = fs::read_to_string(status_path).unwrap_or_default();
            if text.is_empty() {
                return Err("guardian exited before the writer was ready".to_owned());
            }
            return Err(text.trim().to_owned());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("guardian did not become ready".to_owned())
}

fn spawn_direct(request: &WriterRequest, wrap_probe: bool) -> Result<Child, String> {
    let probe_file = if wrap_probe {
        write_probe_file(request.probe_script.as_deref())?
    } else {
        None
    };
    let (program, args) = if probe_file.is_some() && request.probe_out.is_some() {
        let shell = if Path::new("/bin/sh").exists() {
            "/bin/sh"
        } else {
            "sh"
        };
        (shell.to_owned(), probe_shell_args(&request.argv))
    } else {
        (request.argv[0].clone(), request.argv[1..].to_vec())
    };
    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    if request.clear_env {
        command.env_clear();
    }
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &request.env {
        command.env(key, value);
    }
    if let Some(path) = &probe_file {
        command.env("NXR_LEAF_PROBE_FILE", path);
    }
    if let Some(path) = &request.probe_out {
        command.env("NXR_LEAF_PROBE_OUT", path);
    }
    apply_stdio(&mut command, &request.stdio)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|err| format!("spawn {}: {err}", request.argv[0]))
}

fn apply_stdio(command: &mut Command, stdio: &WriterStdio) -> Result<(), String> {
    match stdio {
        WriterStdio::Log(path) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("create log dir {}: {err}", parent.display()))?;
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|err| format!("open log {}: {err}", path.display()))?;
            let err = file
                .try_clone()
                .map_err(|err| format!("clone log {}: {err}", path.display()))?;
            command.stdout(Stdio::from(file));
            command.stderr(Stdio::from(err));
        }
        WriterStdio::Inherit => {
            command.stdout(Stdio::inherit());
            command.stderr(Stdio::inherit());
        }
        WriterStdio::Piped => {
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        }
    }
    Ok(())
}

fn probe_shell_args(argv: &[String]) -> Vec<String> {
    let body = r#"if [ -n "${NXR_LEAF_PROBE_FILE:-}" ] && [ -n "${NXR_LEAF_PROBE_OUT:-}" ]; then
  dir=${NXR_LEAF_PROBE_OUT%/*}
  if [ -x /bin/mkdir ]; then /bin/mkdir -p "$dir" 2>/dev/null || true; else mkdir -p "$dir" 2>/dev/null || true; fi
  if [ -x /bin/sh ]; then /bin/sh "$NXR_LEAF_PROBE_FILE" > "$NXR_LEAF_PROBE_OUT" 2>/dev/null || true; else sh "$NXR_LEAF_PROBE_FILE" > "$NXR_LEAF_PROBE_OUT" 2>/dev/null || true; fi
fi
exec "$@"
"#;
    let mut args = vec!["-c".to_owned(), body.to_owned(), "nxr-probe".to_owned()];
    args.extend(argv.iter().cloned());
    args
}

fn write_probe_file(script: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(script) = script.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = std::env::temp_dir().join(format!(
        "nxr-probe-{}-{}.sh",
        std::process::id(),
        unique_suffix()
    ));
    let mut file = File::create(&path).map_err(|err| format!("create probe file: {err}"))?;
    file.write_all(script.as_bytes())
        .map_err(|err| format!("write probe file: {err}"))?;
    Ok(Some(path))
}

fn join_dirs(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn unique_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Entry point for `NXR_RESOURCE_GUARDIAN=1`.
///
/// # Errors
///
/// Returns an error when the locks or the writer cannot be started. The
/// status file receives `error ...` and no writer is left running.
pub fn run_guardian_from_env() -> Result<(), String> {
    let cancel = guardian_cancel_flag();
    cancel.store(false, Ordering::SeqCst);
    let dirs = std::env::var("NXR_GUARDIAN_DIRS").unwrap_or_default();
    let output_dirs: Vec<PathBuf> = dirs
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    let argv: Vec<String> =
        serde_json::from_str(&std::env::var("NXR_GUARDIAN_ARGV").unwrap_or_default())
            .unwrap_or_default();
    let status_path = std::env::var("NXR_GUARDIAN_STATUS").unwrap_or_default();
    let status = PathBuf::from(&status_path);
    let admissions = match acquire_ordered(&output_dirs) {
        Ok(admissions) => admissions,
        Err(err) => {
            publish_status(&status, &format!("error {err}"));
            return Err(err);
        }
    };
    if let Ok(pause) = std::env::var("NXR_GUARDIAN_PAUSE")
        && !pause.is_empty()
    {
        let pause_path = PathBuf::from(pause);
        let _ = File::create(&pause_path);
        while pause_path.exists() {
            if cancel.load(Ordering::SeqCst) {
                drop(admissions);
                publish_status(
                    &status,
                    "error guardian cancelled before the writer started",
                );
                return Err("guardian cancelled before the writer started".to_owned());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    let request = WriterRequest {
        argv,
        cwd: std::env::var("NXR_GUARDIAN_CWD").ok().map(PathBuf::from),
        env: serde_json::from_str(&std::env::var("NXR_GUARDIAN_ENV").unwrap_or_default())
            .unwrap_or_default(),
        output_dirs,
        stdio: WriterStdio::Inherit,
        probe_out: std::env::var("NXR_GUARDIAN_PROBE_OUT")
            .ok()
            .map(PathBuf::from),
        probe_script: None,
        guardian_bin: None,
        pause_file: None,
        clear_env: std::env::var("NXR_GUARDIAN_CLEAR").ok().as_deref() == Some("1"),
    };
    let mut child = match spawn_guardian_writer(&request) {
        Ok(child) => child,
        Err(err) => {
            drop(admissions);
            publish_status(&status, &format!("error {err}"));
            return Err(err);
        }
    };
    let writer_pid = child.id();
    let started = process_start_time(writer_pid);
    if let Some(probe) = &request.probe_out {
        for _ in 0..40 {
            if probe.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    publish_status(&status, &format!("ready {writer_pid}"));
    let code = wait_for_writer(&mut child, writer_pid, started, &cancel);
    publish_status(&status, &format!("ready {writer_pid}\nexit {code}"));
    drop(admissions);
    std::process::exit(code);
}

fn spawn_guardian_writer(request: &WriterRequest) -> Result<Child, String> {
    if request.argv.is_empty() {
        return Err("writer command is empty".to_owned());
    }
    let mut argv = request.argv.clone();
    if std::env::var("NXR_GUARDIAN_PROBE_FILE").ok().is_some()
        && std::env::var("NXR_GUARDIAN_PROBE_OUT").ok().is_some()
    {
        let shell = if Path::new("/bin/sh").exists() {
            "/bin/sh"
        } else {
            "sh"
        };
        let mut wrapped = vec![shell.to_owned()];
        wrapped.extend(probe_shell_args(&argv));
        argv = wrapped;
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    if request.clear_env {
        command.env_clear();
    }
    for (key, value) in &request.env {
        command.env(key, value);
    }
    command.env_remove("NXR_RESOURCE_GUARDIAN");
    command.env_remove("NXR_GUARDIAN_DIRS");
    command.env_remove("NXR_GUARDIAN_ARGV");
    command.env_remove("NXR_GUARDIAN_STATUS");
    command.env_remove("NXR_GUARDIAN_PAUSE");
    command.env_remove("NXR_GUARDIAN_LOG");
    command.stdin(Stdio::null());
    if let Ok(log) = std::env::var("NXR_GUARDIAN_LOG")
        && !log.is_empty()
    {
        apply_stdio(&mut command, &WriterStdio::Log(PathBuf::from(log)))?;
    } else {
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());
    }
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    if let Ok(probe_file) = std::env::var("NXR_GUARDIAN_PROBE_FILE") {
        command.env("NXR_LEAF_PROBE_FILE", probe_file);
    }
    if let Ok(probe_out) = std::env::var("NXR_GUARDIAN_PROBE_OUT") {
        command.env("NXR_LEAF_PROBE_OUT", probe_out);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|err| format!("spawn {}: {err}", argv[0]))
}

fn wait_for_writer(
    child: &mut Child,
    writer_pid: u32,
    started: Option<u64>,
    cancel: &AtomicBool,
) -> i32 {
    loop {
        if cancel.load(Ordering::SeqCst) {
            signal_writer_group(writer_pid);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                while writer_group_alive(writer_pid, started) != GroupLiveness::Dead {
                    thread::sleep(GUARDIAN_POLL);
                }
                return status.code().unwrap_or(1);
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return 1,
        }
    }
}

fn signal_writer_group(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        if let Ok(raw) = i32::try_from(pid) {
            let _ = killpg(Pid::from_raw(raw), Signal::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn publish_status(path: &Path, text: &str) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, text);
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

    #[test]
    fn a_missing_guardian_does_not_leave_a_writer() {
        let dir = tempfile::tempdir().expect("output dir");
        let err = launch_writer(&WriterRequest {
            argv: vec!["/bin/sleep".into(), "30".into()],
            cwd: None,
            env: Vec::new(),
            output_dirs: vec![dir.path().to_path_buf()],
            stdio: WriterStdio::Inherit,
            probe_out: None,
            probe_script: None,
            guardian_bin: Some(PathBuf::from("/no/such/nxr-guardian")),
            pause_file: None,
            clear_env: false,
        })
        .expect_err("missing guardian");
        assert!(
            err.contains("guardian") || err.contains("spawn") || err.contains("No such"),
            "{err}"
        );
        let held = crate::target_lock::admit_target(dir.path()).expect("lock was released");
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_alias_cannot_take_the_same_lock_twice() {
        let parent = tempfile::tempdir().expect("parent");
        let real = parent.path().join("real");
        std::fs::create_dir(&real).expect("real dir");
        let alias = parent.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("alias");
        let err = acquire_ordered(&[real.clone(), alias]).expect_err("alias");
        assert!(err.contains("already has a running local build"), "{err}");
        let held = crate::target_lock::admit_target(&real).expect("released after failure");
        drop(held);
    }
}
