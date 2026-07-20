//! Killable supervised child sessions for watch generations.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nxr_core::EnvironmentPolicy;

use crate::signals::exit_code_from_status;

/// How a supervised spawn should wire the child's standard streams.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpawnStdio {
    /// Inherit the caller's stdin/stdout/stderr (interactive / serial default).
    #[default]
    Inherit,
    /// Null/closed stdin; pipe stdout and stderr for labeled/multiplex capture.
    ///
    /// Parallel (`-j > 1`) and `--output` / `--events` task runs use this so
    /// multiple children never share caller stdin ownership.
    PipeStdoutStderr,
}

/// A spawned child in its own process group that can be polled or terminated.
#[derive(Debug)]
pub struct ChildSession {
    child: Child,
    pgid: u32,
}

impl ChildSession {
    /// Process group id (equals the child pid after `process_group(0)`).
    #[must_use]
    pub const fn pgid(&self) -> u32 {
        self.pgid
    }

    /// Take ownership of the child's stdout pipe, if it was spawned with pipes.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Take ownership of the child's stderr pipe, if it was spawned with pipes.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Non-blocking wait. Returns [`None`] while the child is still running.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        Ok(self.child.try_wait()?.map(exit_code_from_status))
    }

    /// Block until the child exits.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn wait(mut self) -> io::Result<i32> {
        let status = self.child.wait()?;
        Ok(exit_code_from_status(status))
    }

    /// Send SIGTERM to this session's process group without waiting.
    ///
    /// # Errors
    ///
    /// Propagates kill errors from the OS. On non-Unix platforms, returns
    /// [`io::ErrorKind::Unsupported`].
    pub fn signal_terminate(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            unix::terminate_group(self.pgid)
        }

        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process group SIGTERM is not supported on this platform",
            ))
        }
    }

    /// Send SIGKILL to this session's process group without waiting.
    ///
    /// # Errors
    ///
    /// Propagates kill errors from the OS. On non-Unix platforms, returns
    /// [`io::ErrorKind::Unsupported`].
    pub fn signal_kill(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            unix::kill_group(self.pgid)
        }

        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process group SIGKILL is not supported on this platform",
            ))
        }
    }

    /// Send SIGTERM to the process group, wait briefly, then SIGKILL if needed.
    ///
    /// # Errors
    ///
    /// Propagates wait or kill errors from the OS.
    pub fn terminate(mut self) -> io::Result<Option<i32>> {
        #[cfg(unix)]
        {
            self.signal_terminate()?;
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok(Some(exit_code_from_status(status)));
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            self.signal_kill()?;
            let status = self.child.wait()?;
            Ok(Some(exit_code_from_status(status)))
        }

        #[cfg(not(unix))]
        {
            let _ = &mut self;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process group terminate is not supported on this platform",
            ))
        }
    }
}

/// Spawn `program` with argv in a new process group (does not wait).
///
/// Uses [`SpawnStdio::Inherit`] for all streams.
///
/// # Errors
///
/// Returns an error if the child cannot be spawned. On Windows, returns
/// [`io::ErrorKind::Unsupported`].
pub fn spawn_in<P, A>(
    program: P,
    args: &[A],
    cwd: Option<&Path>,
    environment: &EnvironmentPolicy,
) -> io::Result<ChildSession>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    spawn_in_with(program, args, cwd, environment, SpawnStdio::Inherit)
}

/// Spawn `program` with argv in a new process group and the given stdio mode.
///
/// # Errors
///
/// Returns an error if the child cannot be spawned. On Windows, returns
/// [`io::ErrorKind::Unsupported`].
pub fn spawn_in_with<P, A>(
    program: P,
    args: &[A],
    cwd: Option<&Path>,
    environment: &EnvironmentPolicy,
    stdio: SpawnStdio,
) -> io::Result<ChildSession>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    #[cfg(unix)]
    {
        unix::spawn(program.as_ref(), args, cwd, environment, stdio)
    }

    #[cfg(windows)]
    {
        let _ = (program, args, cwd, environment, stdio);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows supervised spawn is not implemented yet",
        ))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (program, args, cwd, environment, stdio);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "supervised spawn is not supported on this platform",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use super::{ChildSession, Command, EnvironmentPolicy, OsStr, Path, SpawnStdio, Stdio, io};
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    use std::os::unix::process::CommandExt;

    pub(super) fn spawn<A: AsRef<OsStr>>(
        program: &OsStr,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
        stdio: SpawnStdio,
    ) -> io::Result<ChildSession> {
        let (child_stdin, child_stdout, child_stderr) = match stdio {
            SpawnStdio::Inherit => (Stdio::inherit(), Stdio::inherit(), Stdio::inherit()),
            SpawnStdio::PipeStdoutStderr => (Stdio::null(), Stdio::piped(), Stdio::piped()),
        };

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(child_stdin)
            .stdout(child_stdout)
            .stderr(child_stderr)
            .process_group(0);

        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        environment.apply(&mut command);

        let child = command.spawn()?;
        let pgid = child.id();
        Ok(ChildSession { child, pgid })
    }

    pub(super) fn terminate_group(pgid: u32) -> io::Result<()> {
        signal_group(pgid, Signal::SIGTERM)
    }

    pub(super) fn kill_group(pgid: u32) -> io::Result<()> {
        signal_group(pgid, Signal::SIGKILL)
    }

    fn signal_group(pgid: u32, signal: Signal) -> io::Result<()> {
        let group = Pid::from_raw(i32::try_from(pgid).unwrap_or(0));
        if group.as_raw() <= 0 {
            return Ok(());
        }
        match killpg(group, signal) {
            // ESRCH: already gone — treat as success for terminate paths.
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(err) => Err(io::Error::from(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use nxr_core::EnvironmentPolicy;

    use super::{ChildSession, SpawnStdio, spawn_in, spawn_in_with};

    #[cfg(unix)]
    fn group_alive(pgid: u32) -> bool {
        use nix::sys::signal::killpg;
        use nix::unistd::Pid;

        let group = Pid::from_raw(i32::try_from(pgid).unwrap_or(0));
        if group.as_raw() <= 0 {
            return false;
        }
        killpg(group, None).is_ok()
    }

    #[cfg(unix)]
    fn unix_util(name: &str) -> String {
        for prefix in ["/usr/bin", "/bin"] {
            let candidate = format!("{prefix}/{name}");
            if Path::new(&candidate).exists() {
                return candidate;
            }
        }
        panic!("missing {name} under /usr/bin or /bin");
    }

    #[cfg(unix)]
    #[test]
    fn spawn_true_can_try_wait() {
        let mut session = spawn_in(
            unix_util("true"),
            &[] as &[&str],
            None,
            &EnvironmentPolicy::Inherit,
        )
        .expect("spawn true");
        let mut code = None;
        for _ in 0..50 {
            if let Some(c) = session.try_wait().expect("try_wait") {
                code = Some(c);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(code, Some(0));
    }

    /// Piped/multiplex mode must not inherit caller stdin (would hang on `read`).
    #[cfg(unix)]
    #[test]
    fn pipe_stdout_stderr_uses_null_stdin() {
        let bash = unix_util("bash");
        // Null stdin: not a TTY, and a blocking read returns EOF immediately.
        let script =
            "if [ -t 0 ]; then exit 10; fi; if IFS= read -r _; then exit 11; else exit 0; fi";
        let mut session = spawn_in_with(
            &bash,
            &["-c", script],
            None,
            &EnvironmentPolicy::Inherit,
            SpawnStdio::PipeStdoutStderr,
        )
        .expect("spawn bash stdin probe");
        let mut code = None;
        for _ in 0..100 {
            if let Some(c) = session.try_wait().expect("try_wait") {
                code = Some(c);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(code, Some(0), "child should see closed stdin, not hang");
    }

    #[cfg(unix)]
    #[test]
    fn terminate_stops_sleep() {
        let session = spawn_in(
            unix_util("sleep"),
            &["30"],
            None,
            &EnvironmentPolicy::Inherit,
        )
        .expect("spawn sleep");
        let code = session.terminate().expect("terminate").expect("exit code");
        // SIGTERM → 128+15, or SIGKILL → 128+9 if escalate path runs.
        assert!(
            code == 128 + 15 || code == 128 + 9,
            "unexpected code {code}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_pgid_matches_child() {
        let session: ChildSession = spawn_in(
            unix_util("sleep"),
            &["1"],
            None,
            &EnvironmentPolicy::Inherit,
        )
        .expect("spawn");
        assert!(session.pgid() > 0);
        let _ = session.terminate();
    }

    #[cfg(unix)]
    #[test]
    fn terminate_escalates_when_child_ignores_sigterm() {
        let bash = unix_util("bash");
        let ignore_term = ["-c", "trap '' TERM; while true; do sleep 60; done"];
        let session = spawn_in(&bash, &ignore_term, None, &EnvironmentPolicy::Inherit)
            .expect("spawn bash ignoring SIGTERM");
        let pgid = session.pgid();
        thread::sleep(Duration::from_millis(100));

        let code = session.terminate().expect("terminate").expect("exit code");
        assert_eq!(code, 128 + 9, "expected SIGKILL after grace, got {code}");
        assert!(!group_alive(pgid), "process group {pgid} still alive");
    }

    #[cfg(unix)]
    #[test]
    fn grandchildren_in_process_group_are_terminated() {
        let bash = unix_util("bash");
        // Nested shell keeps the leaf `sleep` in the supervised process group.
        let nested = ["-c", "bash -c 'exec sleep 60'"];
        let session = spawn_in(&bash, &nested, None, &EnvironmentPolicy::Inherit).expect("spawn");
        let pgid = session.pgid();

        let code = session.terminate().expect("terminate").expect("exit code");
        assert!(
            code == 128 + 15 || code == 128 + 9,
            "unexpected exit code {code}"
        );
        assert!(
            !group_alive(pgid),
            "grandchild left process group {pgid} alive"
        );
    }
}
