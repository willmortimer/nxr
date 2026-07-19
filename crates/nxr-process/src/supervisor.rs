//! Multi-child process-group supervision.
//!
//! Tracks N [`ChildSession`]s (each in its own process group) for parallel
//! tasks and watch-style orchestration. Unix-first; Windows APIs return
//! [`std::io::ErrorKind::Unsupported`].
//!
//! # Interrupt escalation
//!
//! When driving shutdown from [`InterruptFlags`] via
//! [`Supervisor::handle_interrupt`]:
//!
//! 1. **First** pending interrupt → graceful [`shutdown_all`]: SIGTERM every
//!    live process group, then SIGKILL any survivors after `grace`.
//! 2. **Second** interrupt while that grace window is still open → escalate
//!    immediately with SIGKILL (no further waiting on the grace deadline).
//!
//! Callers that manage interrupts themselves can use [`shutdown_all`] /
//! [`kill_all`] directly; the two-strike policy only applies through
//! [`handle_interrupt`].

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use nxr_core::EnvironmentPolicy;

use crate::session::{ChildSession, spawn_in};
use crate::signals::InterruptFlags;

/// Coordinates multiple supervised children and coordinated group shutdown.
#[derive(Debug, Default)]
pub struct Supervisor {
    children: Vec<ChildSession>,
    /// Set after the first interrupt has begun graceful shutdown.
    interrupt_armed: bool,
}

impl Supervisor {
    /// Create an empty supervisor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of children still tracked (not yet reaped).
    #[must_use]
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether no children are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Process group ids of currently tracked children.
    #[must_use]
    pub fn pgids(&self) -> Vec<u32> {
        self.children.iter().map(ChildSession::pgid).collect()
    }

    /// Take ownership of an already-spawned session.
    pub fn add(&mut self, session: ChildSession) {
        self.children.push(session);
    }

    /// Spawn via [`spawn_in`] and track the resulting session.
    ///
    /// # Errors
    ///
    /// Propagates spawn errors. On Windows, returns
    /// [`io::ErrorKind::Unsupported`].
    pub fn spawn<P, A>(
        &mut self,
        program: P,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
    ) -> io::Result<u32>
    where
        P: AsRef<OsStr>,
        A: AsRef<OsStr>,
    {
        let session = spawn_in(program, args, cwd, environment)?;
        let pgid = session.pgid();
        self.add(session);
        Ok(pgid)
    }

    /// Non-blocking poll: reap the first child that has exited.
    ///
    /// Returns that child's exit code (shell convention for signals) and
    /// removes it from the supervisor.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn try_wait_any(&mut self) -> io::Result<Option<i32>> {
        for index in 0..self.children.len() {
            if let Some(code) = self.children[index].try_wait()? {
                self.children.swap_remove(index);
                return Ok(Some(code));
            }
        }
        Ok(None)
    }

    /// Poll every child once; reap and collect exit codes for those that exited.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn try_wait_all(&mut self) -> io::Result<Vec<i32>> {
        let mut codes = Vec::new();
        let mut index = 0;
        while index < self.children.len() {
            if let Some(code) = self.children[index].try_wait()? {
                self.children.swap_remove(index);
                codes.push(code);
            } else {
                index += 1;
            }
        }
        Ok(codes)
    }

    /// Graceful shutdown: SIGTERM all groups, wait up to `grace`, then SIGKILL.
    ///
    /// Reaps every child and returns their exit codes (order is not significant).
    /// Clears the interrupt-armed state.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors. On non-Unix platforms with live
    /// children, returns [`io::ErrorKind::Unsupported`].
    pub fn shutdown_all(&mut self, grace: Duration) -> io::Result<Vec<i32>> {
        let codes = self.shutdown_all_inner(grace, None)?;
        self.interrupt_armed = false;
        Ok(codes)
    }

    /// Immediate SIGKILL of every live process group, then reap.
    ///
    /// Clears the interrupt-armed state.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors. On non-Unix platforms with live
    /// children, returns [`io::ErrorKind::Unsupported`].
    pub fn kill_all(&mut self) -> io::Result<Vec<i32>> {
        let codes = self.kill_remaining()?;
        self.interrupt_armed = false;
        Ok(codes)
    }

    /// Apply the two-strike interrupt policy using `flags`.
    ///
    /// - No pending interrupt → [`None`], no action.
    /// - First interrupt (supervisor not yet armed) → arm, run graceful
    ///   [`shutdown_all`] while watching `flags` for a second strike during
    ///   `grace`; returns the collected exit codes.
    /// - Second interrupt while already armed (e.g. caller polls again after a
    ///   partial external shutdown) → immediate [`kill_all`].
    ///
    /// During the grace window of the first strike, a second pending interrupt
    /// escalates to SIGKILL without waiting out the remaining grace.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors from shutdown.
    pub fn handle_interrupt(
        &mut self,
        flags: &InterruptFlags,
        grace: Duration,
    ) -> io::Result<Option<Vec<i32>>> {
        if self.interrupt_armed {
            if !flags.take_pending() {
                return Ok(None);
            }
            return Ok(Some(self.kill_all()?));
        }

        if !flags.take_pending() {
            return Ok(None);
        }

        self.interrupt_armed = true;
        let codes = self.shutdown_all_inner(grace, Some(flags))?;
        self.interrupt_armed = false;
        Ok(Some(codes))
    }

    fn shutdown_all_inner(
        &mut self,
        grace: Duration,
        flags: Option<&InterruptFlags>,
    ) -> io::Result<Vec<i32>> {
        if self.children.is_empty() {
            return Ok(Vec::new());
        }

        #[cfg(not(unix))]
        {
            let _ = (grace, flags);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "multi-child supervisor shutdown is not supported on this platform",
            ));
        }

        #[cfg(unix)]
        {
            for child in &self.children {
                child.signal_terminate()?;
            }

            let deadline = Instant::now() + grace;
            let mut codes = Vec::new();

            loop {
                if let Some(flags) = flags
                    && flags.take_pending()
                {
                    codes.extend(self.kill_remaining()?);
                    return Ok(codes);
                }

                codes.extend(self.try_wait_all()?);
                if self.children.is_empty() {
                    return Ok(codes);
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            codes.extend(self.kill_remaining()?);
            Ok(codes)
        }
    }

    fn kill_remaining(&mut self) -> io::Result<Vec<i32>> {
        if self.children.is_empty() {
            return Ok(Vec::new());
        }

        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "multi-child supervisor kill is not supported on this platform",
            ))
        }

        #[cfg(unix)]
        {
            for child in &self.children {
                child.signal_kill()?;
            }

            let mut codes = Vec::with_capacity(self.children.len());
            // Take ownership so we can `wait` each session to completion.
            let sessions = std::mem::take(&mut self.children);
            for session in sessions {
                codes.push(session.wait()?);
            }
            Ok(codes)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    use nxr_core::EnvironmentPolicy;

    use super::Supervisor;
    use crate::signals::InterruptFlags;

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
    fn group_alive(pgid: u32) -> bool {
        use nix::sys::signal::killpg;
        use nix::unistd::Pid;

        let group = Pid::from_raw(i32::try_from(pgid).unwrap_or(0));
        if group.as_raw() <= 0 {
            return false;
        }
        // Signal 0 probes existence without delivering a real signal.
        killpg(group, None).is_ok()
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_all_stops_two_sleeps() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let sleep = unix_util("sleep");

        let pgid_a = supervisor
            .spawn(&sleep, &["30"], None, &env)
            .expect("spawn sleep a");
        let pgid_b = supervisor
            .spawn(&sleep, &["30"], None, &env)
            .expect("spawn sleep b");
        assert_eq!(supervisor.len(), 2);

        let codes = supervisor
            .shutdown_all(Duration::from_secs(2))
            .expect("shutdown_all");
        assert_eq!(codes.len(), 2);
        assert!(supervisor.is_empty());
        for code in codes {
            assert!(
                code == 128 + 15 || code == 128 + 9,
                "unexpected exit code {code}"
            );
        }
        assert!(!group_alive(pgid_a), "group {pgid_a} still alive");
        assert!(!group_alive(pgid_b), "group {pgid_b} still alive");
    }

    #[cfg(unix)]
    #[test]
    fn try_wait_any_reaps_finished_child() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        supervisor
            .spawn(unix_util("true"), &[] as &[&str], None, &env)
            .expect("spawn true");
        supervisor
            .spawn(unix_util("sleep"), &["30"], None, &env)
            .expect("spawn sleep");

        let mut finished = None;
        for _ in 0..50 {
            if let Some(code) = supervisor.try_wait_any().expect("try_wait_any") {
                finished = Some(code);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(finished, Some(0));
        assert_eq!(supervisor.len(), 1);

        let _ = supervisor.shutdown_all(Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn handle_interrupt_escalates_on_second_strike() {
        let flags = InterruptFlags::install().expect("install flags");
        let escalate = flags.trigger_handle();
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let bash = unix_util("bash");

        // Ignore SIGTERM in the shell itself and keep looping so a group SIGTERM
        // that kills inner `sleep` does not exit the supervised process.
        let ignore_term = ["-c", "trap '' TERM; while true; do sleep 60; done"];
        let pgid_a = supervisor
            .spawn(&bash, &ignore_term, None, &env)
            .expect("spawn a");
        let pgid_b = supervisor
            .spawn(&bash, &ignore_term, None, &env)
            .expect("spawn b");
        // Allow shells to install the TERM trap before the first SIGTERM.
        thread::sleep(Duration::from_millis(100));

        let joiner = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            escalate();
        });

        flags.trigger_for_test();
        let started = Instant::now();
        let codes = supervisor
            .handle_interrupt(&flags, Duration::from_secs(10))
            .expect("handle_interrupt")
            .expect("shutdown ran");
        joiner.join().expect("escalate thread");

        assert!(
            started.elapsed() < Duration::from_secs(3),
            "escalation should not wait out the full grace window"
        );
        assert_eq!(codes.len(), 2);
        for code in codes {
            assert_eq!(code, 128 + 9, "expected SIGKILL exit, got {code}");
        }
        assert!(supervisor.is_empty());
        assert!(!group_alive(pgid_a));
        assert!(!group_alive(pgid_b));
    }

    #[cfg(unix)]
    #[test]
    fn kill_all_escalates_immediately() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let bash = unix_util("bash");
        let ignore_term = ["-c", "trap '' TERM; while true; do sleep 60; done"];

        let pgid = supervisor
            .spawn(&bash, &ignore_term, None, &env)
            .expect("spawn");
        thread::sleep(Duration::from_millis(100));

        let codes = supervisor.kill_all().expect("kill_all");
        assert_eq!(codes, vec![128 + 9]);
        assert!(!group_alive(pgid));
    }
}
