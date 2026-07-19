//! Foreground child execution (supervised spawn, not `exec`).

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use nxr_core::EnvironmentPolicy;

use crate::signals::exit_code_from_status;

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
    #[cfg(unix)]
    {
        unix::run(program.as_ref(), args, cwd, environment)
    }

    #[cfg(windows)]
    {
        let _ = (program, args, cwd, environment);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows foreground supervision is not implemented yet",
        ))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (program, args, cwd, environment);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "foreground supervision is not supported on this platform",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use super::{
        Command, Duration, EnvironmentPolicy, OsStr, Path, Stdio, exit_code_from_status, io, thread,
    };
    use crate::signals::unix::SignalForwarder;
    use std::os::unix::process::CommandExt;

    pub(super) fn run<A: AsRef<OsStr>>(
        program: &OsStr,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
    ) -> io::Result<i32> {
        let forwarder = SignalForwarder::install()?;

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            // New process group (pgid == child pid) so we can forward signals
            // without relying on the terminal's foreground group alone.
            .process_group(0);

        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        environment.apply(&mut command);

        let mut child = command.spawn()?;
        let pgid = child.id();

        let status = loop {
            forwarder.poll_and_forward(pgid);
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    // Short sleep so signal flags are observed promptly without
                    // busy-spinning; `Child::wait` retries EINTR and would miss
                    // our flag-based forwarder.
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };

        Ok(exit_code_from_status(status))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use nxr_core::EnvironmentPolicy;

    use super::{run, run_in};

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
    fn true_exits_zero() {
        let code = run(unix_util("true"), &[] as &[&str]).expect("run true");
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn false_exits_one() {
        let code = run(unix_util("false"), &[] as &[&str]).expect("run false");
        assert_eq!(code, 1);
    }

    #[cfg(unix)]
    #[test]
    fn no_shell_evaluation_of_args() {
        // If args were shell-evaluated, `&& exit 99` would change the status.
        // `true` ignores extra argv and still exits 0.
        let code = run(unix_util("true"), &["&&", "exit", "99"]).expect("run");
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
        let code =
            run_in(unix_util("printenv"), &["NXR_CLEAN_TEST"], None, &policy).expect("printenv");
        // printenv exits 0 when the variable is set
        assert_eq!(code, 0);
    }
}
