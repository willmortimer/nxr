//! Foreground child execution (supervised spawn, not `exec`).
//!
//! ## CI / PTY limits
//!
//! Headless CI runners have no controlling TTY, so PTY allocation and
//! `SIGWINCH` (terminal resize) forwarding are not covered by automated tests
//! here. Signal forwarding (`SIGINT` / `SIGTERM`) and process-group shutdown
//! are exercised instead; interactive resize behavior is manual / future harness
//! work (see `docs/ARCHITECTURE.md`).
//!
//! ## Stderr capture policy
//!
//! When the runner's stderr is a terminal, the child inherits stderr so Nix and
//! apps keep TTY-aware rendering. Captured stderr is empty in that mode, so
//! missing-installable suggestions are skipped. When stderr is not a terminal
//! (pipes, CI), stderr is tee'd with a bounded rolling tail for diagnostics.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use nxr_core::EnvironmentPolicy;

use crate::signals::exit_code_from_status;

/// Rolling stderr tail retained for missing-installable detection (non-TTY only).
pub const STDERR_TAIL_CAPACITY: usize = 128 * 1024;

/// Spawn `program` with an argv vector (no shell), inherit stdio, and wait.
///
/// On Unix the child is placed in its own process group; SIGINT and SIGTERM
/// received by the runner are forwarded to that group. The returned code is the
/// child's exit status, or `128 + signal` when the child dies from a signal.
///
/// This is supervised spawn: the runner process continues after the child exits
/// (unlike `exec`), so callers can emit summaries or continue orchestration.
///
/// # Errors
///
/// Returns an error if the child cannot be spawned or waited on. On Windows,
/// returns [`io::ErrorKind::Unsupported`] until job-object supervision lands.
pub fn run<P, A>(program: P, args: &[A]) -> io::Result<i32>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    run_in(program, args, None, &EnvironmentPolicy::Inherit)
}

/// Like [`run`], but optionally sets the child working directory and environment policy.
///
/// # Errors
///
/// Same as [`run`].
pub fn run_in<P, A>(
    program: P,
    args: &[A],
    cwd: Option<&Path>,
    environment: &EnvironmentPolicy,
) -> io::Result<i32>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    Ok(run_in_with_stderr(program, args, cwd, environment, None)?.0)
}

/// Like [`run_in`], but may return a bounded stderr tail for diagnostics.
///
/// When stderr is a TTY, the child inherits stderr and the returned string is
/// empty. Otherwise stderr is tee'd to the inherited stream and a rolling tail
/// of at most [`STDERR_TAIL_CAPACITY`] bytes is retained.
///
/// # Errors
///
/// Same as [`run`].
pub fn run_in_with_stderr<P, A>(
    program: P,
    args: &[A],
    cwd: Option<&Path>,
    environment: &EnvironmentPolicy,
    env_overrides: Option<&BTreeMap<String, String>>,
) -> io::Result<(i32, String)>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    #[cfg(unix)]
    {
        unix::run_with_stderr(program.as_ref(), args, cwd, environment, env_overrides)
    }

    #[cfg(windows)]
    {
        let _ = (program, args, cwd, environment, env_overrides);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows foreground supervision is not implemented yet",
        ))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (program, args, cwd, environment, env_overrides);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "foreground supervision is not supported on this platform",
        ))
    }
}

/// Like [`run_in_with_stderr`], but always pipes stderr and invokes `on_line` for
/// each complete line (without the trailing newline). The returned string is a
/// rolling raw-stderr tail for diagnostics.
///
/// # Errors
///
/// Same as [`run`].
pub fn run_in_with_stderr_lines<P, A, F>(
    program: P,
    args: &[A],
    cwd: Option<&Path>,
    environment: &EnvironmentPolicy,
    env_overrides: Option<&BTreeMap<String, String>>,
    on_line: F,
) -> io::Result<(i32, String)>
where
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
    F: FnMut(&str) + Send + 'static,
{
    #[cfg(unix)]
    {
        unix::run_with_stderr_lines(
            program.as_ref(),
            args,
            cwd,
            environment,
            env_overrides,
            on_line,
        )
    }

    #[cfg(windows)]
    {
        let _ = (program, args, cwd, environment, env_overrides, on_line);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows foreground supervision is not implemented yet",
        ))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (program, args, cwd, environment, env_overrides, on_line);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "foreground supervision is not supported on this platform",
        ))
    }
}

/// Append `chunk` to `captured`, keeping only the last `capacity` bytes.
pub fn append_rolling_tail(captured: &mut Vec<u8>, chunk: &[u8], capacity: usize) {
    if capacity == 0 {
        captured.clear();
        return;
    }
    if chunk.len() >= capacity {
        captured.clear();
        captured.extend_from_slice(&chunk[chunk.len() - capacity..]);
        return;
    }
    let overflow = captured
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(capacity);
    if overflow > 0 {
        captured.drain(..overflow);
    }
    captured.extend_from_slice(chunk);
}

#[cfg(unix)]
mod unix {
    use super::{
        BTreeMap, Command, Duration, EnvironmentPolicy, IsTerminal, OsStr, Path,
        STDERR_TAIL_CAPACITY, Stdio, append_rolling_tail, exit_code_from_status, io, thread,
    };
    use crate::signals::unix::SignalForwarder;
    use std::os::unix::process::CommandExt;

    #[allow(dead_code)]
    pub(super) fn run<A: AsRef<OsStr>>(
        program: &OsStr,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
    ) -> io::Result<i32> {
        Ok(run_with_stderr(program, args, cwd, environment, None)?.0)
    }

    pub(super) fn run_with_stderr<A: AsRef<OsStr>>(
        program: &OsStr,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
        env_overrides: Option<&BTreeMap<String, String>>,
    ) -> io::Result<(i32, String)> {
        use std::io::{Read, Write};

        let forwarder = SignalForwarder::install()?;
        let capture_stderr = !io::stderr().is_terminal();

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .process_group(0);

        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        environment.apply_with_overrides(&mut command, env_overrides.unwrap_or(&BTreeMap::new()));

        let mut child = command.spawn()?;
        let program_str = program.to_string_lossy();
        if program_str == "nix" || program_str.ends_with("/nix") {
            nxr_core::record_nix_spawn();
        }
        let pgid = child.id();
        let spawn_started = nxr_core::perf_enabled().then(std::time::Instant::now);

        let tee = if capture_stderr {
            let mut stderr_pipe = child.stderr.take().expect("piped stderr");
            Some(thread::spawn(move || {
                let mut captured = Vec::new();
                let mut buf = [0_u8; 8192];
                let mut real_stderr = io::stderr();
                let mut first_output_recorded = false;
                loop {
                    match stderr_pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if !first_output_recorded {
                                if let Some(started) = spawn_started {
                                    nxr_core::record_spawn_to_child_output_us(
                                        started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                                    );
                                }
                                first_output_recorded = true;
                            }
                            let chunk = &buf[..n];
                            append_rolling_tail(&mut captured, chunk, STDERR_TAIL_CAPACITY);
                            let _ = real_stderr.write_all(chunk);
                        }
                    }
                }
                let _ = real_stderr.flush();
                String::from_utf8_lossy(&captured).into_owned()
            }))
        } else {
            None
        };

        let status = loop {
            forwarder.poll_and_forward(pgid);
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };

        let stderr = tee
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default();
        Ok((exit_code_from_status(status), stderr))
    }

    pub(super) fn run_with_stderr_lines<A, F>(
        program: &OsStr,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
        env_overrides: Option<&BTreeMap<String, String>>,
        mut on_line: F,
    ) -> io::Result<(i32, String)>
    where
        A: AsRef<OsStr>,
        F: FnMut(&str) + Send + 'static,
    {
        use std::io::{BufRead, BufReader, Write};

        let forwarder = SignalForwarder::install()?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .process_group(0);

        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        environment.apply_with_overrides(&mut command, env_overrides.unwrap_or(&BTreeMap::new()));

        let mut child = command.spawn()?;
        let program_str = program.to_string_lossy();
        if program_str == "nix" || program_str.ends_with("/nix") {
            nxr_core::record_nix_spawn();
        }
        let pgid = child.id();
        let spawn_started = nxr_core::perf_enabled().then(std::time::Instant::now);

        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let tee = thread::spawn(move || {
            let mut captured = Vec::new();
            let mut reader = BufReader::new(&mut stderr_pipe);
            let mut line = String::new();
            let mut first_output_recorded = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if !first_output_recorded {
                            if let Some(started) = spawn_started {
                                nxr_core::record_spawn_to_child_output_us(
                                    started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                                );
                            }
                            first_output_recorded = true;
                        }
                        append_rolling_tail(&mut captured, line.as_bytes(), STDERR_TAIL_CAPACITY);
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        on_line(trimmed);
                    }
                }
            }
            // Flush any trailing CR-updated status line on TTYs.
            if io::stderr().is_terminal() {
                let mut real_stderr = io::stderr();
                let _ = write!(real_stderr, "\r\x1b[2K");
                let _ = real_stderr.flush();
            }
            String::from_utf8_lossy(&captured).into_owned()
        });

        let status = loop {
            forwarder.poll_and_forward(pgid);
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };

        let stderr = tee.join().unwrap_or_default();
        Ok((exit_code_from_status(status), stderr))
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use nxr_core::EnvironmentPolicy;

    use super::{append_rolling_tail, run, run_in};

    #[test]
    fn rolling_tail_keeps_only_capacity() {
        let mut captured = Vec::new();
        append_rolling_tail(&mut captured, b"abcdefghij", 4);
        assert_eq!(&captured, b"ghij");
        append_rolling_tail(&mut captured, b"KL", 4);
        assert_eq!(&captured, b"ijKL");
        append_rolling_tail(&mut captured, b"XXXXXXXXXXXX", 4);
        assert_eq!(&captured, b"XXXX");
    }

    #[cfg(unix)]
    #[test]
    fn true_exits_zero() {
        let code = run(crate::test_unix::unix_util("true"), &[] as &[&str]).expect("run true");
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn false_exits_one() {
        let code = run(crate::test_unix::unix_util("false"), &[] as &[&str]).expect("run false");
        assert_eq!(code, 1);
    }

    #[cfg(unix)]
    #[test]
    fn no_shell_evaluation_of_args() {
        // If args were shell-evaluated, `&& exit 99` would change the status.
        // `true` ignores extra argv and still exits 0.
        let code = run(crate::test_unix::unix_util("true"), &["&&", "exit", "99"]).expect("run");
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_failure_is_error() {
        let err = run("/nonexistent/nxr-process-test-bin", &[] as &[&str])
            .expect_err("missing binary should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn clean_env_can_set_and_unset() {
        let policy = EnvironmentPolicy::clean(
            [],
            [("NXR_CLEAN_TEST".to_owned(), "1".to_owned())],
            ["PATH".to_owned()],
        );
        let code = run_in(
            crate::test_unix::unix_util("printenv"),
            &["NXR_CLEAN_TEST"],
            None,
            &policy,
        )
        .expect("printenv");
        // printenv exits 0 when the variable is set
        assert_eq!(code, 0);
    }

    /// End-to-end: runner SIGINT handler forwards to the supervised child group.
    #[cfg(unix)]
    #[test]
    fn sigint_stops_foreground_child() {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        let sleep = crate::test_unix::unix_util("sleep");
        let handle = thread::spawn(move || run(&sleep, &["60"]));

        thread::sleep(Duration::from_millis(150));
        kill(
            Pid::from_raw(i32::try_from(std::process::id()).expect("pid fits i32")),
            Signal::SIGINT,
        )
        .expect("SIGINT to test process");

        let code = handle.join().expect("join run thread").expect("run");
        assert_eq!(
            code,
            128 + Signal::SIGINT as i32,
            "child should die from forwarded SIGINT, got {code}"
        );
    }
}
