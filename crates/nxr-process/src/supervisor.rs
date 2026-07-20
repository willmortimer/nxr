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

use crate::session::{ChildSession, SpawnStdio, spawn_in_with};
use crate::signals::InterruptFlags;

/// One supervised child with a caller-facing identity (task node id, etc.).
#[derive(Debug)]
struct TrackedChild {
    id: String,
    session: ChildSession,
}

/// Coordinates multiple supervised children and coordinated group shutdown.
#[derive(Debug, Default)]
pub struct Supervisor {
    children: Vec<TrackedChild>,
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
        self.children
            .iter()
            .map(|child| child.session.pgid())
            .collect()
    }

    /// Ids of currently tracked children (lexicographic insertion order).
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.children.iter().map(|child| child.id.as_str())
    }

    /// Take ownership of an already-spawned session under `id`.
    pub fn add(&mut self, id: impl Into<String>, session: ChildSession) {
        self.children.push(TrackedChild {
            id: id.into(),
            session,
        });
    }

    /// Spawn via [`spawn_in_with`] with inherited stdio and track under `id`.
    ///
    /// # Errors
    ///
    /// Propagates spawn errors. On Windows, returns
    /// [`io::ErrorKind::Unsupported`].
    pub fn spawn<P, A>(
        &mut self,
        id: impl Into<String>,
        program: P,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
    ) -> io::Result<u32>
    where
        P: AsRef<OsStr>,
        A: AsRef<OsStr>,
    {
        self.spawn_with(id, program, args, cwd, environment, SpawnStdio::Inherit)
    }

    /// Spawn with explicit stdio mode and track under `id`.
    ///
    /// When `stdio` is [`SpawnStdio::PipeStdoutStderr`], call
    /// [`ChildSession::take_stdout`] / [`take_stderr`] on the session before
    /// adding it if you need the pipes — prefer [`Self::spawn_piped`] which
    /// returns them.
    ///
    /// # Errors
    ///
    /// Propagates spawn errors.
    pub fn spawn_with<P, A>(
        &mut self,
        id: impl Into<String>,
        program: P,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
        stdio: SpawnStdio,
    ) -> io::Result<u32>
    where
        P: AsRef<OsStr>,
        A: AsRef<OsStr>,
    {
        let session = spawn_in_with(program, args, cwd, environment, stdio)?;
        let pgid = session.pgid();
        self.add(id, session);
        Ok(pgid)
    }

    /// Spawn with piped stdout/stderr and null stdin, return the pipes, and
    /// track under `id`.
    ///
    /// Stdin is closed so parallel/multiplex children never share caller stdin.
    ///
    /// # Errors
    ///
    /// Propagates spawn errors.
    pub fn spawn_piped<P, A>(
        &mut self,
        id: impl Into<String>,
        program: P,
        args: &[A],
        cwd: Option<&Path>,
        environment: &EnvironmentPolicy,
    ) -> io::Result<(u32, std::process::ChildStdout, std::process::ChildStderr)>
    where
        P: AsRef<OsStr>,
        A: AsRef<OsStr>,
    {
        let mut session = spawn_in_with(
            program,
            args,
            cwd,
            environment,
            SpawnStdio::PipeStdoutStderr,
        )?;
        let stdout = session
            .take_stdout()
            .ok_or_else(|| io::Error::other("spawned child missing stdout pipe"))?;
        let stderr = session
            .take_stderr()
            .ok_or_else(|| io::Error::other("spawned child missing stderr pipe"))?;
        let pgid = session.pgid();
        self.add(id, session);
        Ok((pgid, stdout, stderr))
    }

    /// Non-blocking poll: reap the first child that has exited.
    ///
    /// Returns that child's id and exit code (shell convention for signals)
    /// and removes it from the supervisor.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn try_wait_any(&mut self) -> io::Result<Option<(String, i32)>> {
        for index in 0..self.children.len() {
            if let Some(code) = self.children[index].session.try_wait()? {
                let TrackedChild { id, .. } = self.children.swap_remove(index);
                return Ok(Some((id, code)));
            }
        }
        Ok(None)
    }

    /// Poll every child once; reap and collect `(id, code)` for those that exited.
    ///
    /// # Errors
    ///
    /// Propagates wait errors from the OS.
    pub fn try_wait_all(&mut self) -> io::Result<Vec<(String, i32)>> {
        let mut codes = Vec::new();
        let mut index = 0;
        while index < self.children.len() {
            if let Some(code) = self.children[index].session.try_wait()? {
                let TrackedChild { id, .. } = self.children.swap_remove(index);
                codes.push((id, code));
            } else {
                index += 1;
            }
        }
        Ok(codes)
    }

    /// Graceful shutdown: SIGTERM all groups, wait up to `grace`, then SIGKILL.
    ///
    /// Reaps every child and returns their `(id, exit code)` pairs (order is
    /// not significant). Clears the interrupt-armed state.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors. On non-Unix platforms with live
    /// children, returns [`io::ErrorKind::Unsupported`].
    pub fn shutdown_all(&mut self, grace: Duration) -> io::Result<Vec<(String, i32)>> {
        let codes = self.shutdown_all_inner(grace, None)?;
        self.interrupt_armed = false;
        Ok(codes)
    }

    /// Graceful shutdown of a single tracked child by id.
    ///
    /// Returns `Ok(None)` when `id` is not currently tracked.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors.
    pub fn shutdown_one(&mut self, id: &str, grace: Duration) -> io::Result<Option<i32>> {
        let Some(index) = self.children.iter().position(|child| child.id == id) else {
            return Ok(None);
        };
        let mut child = self.children.swap_remove(index);
        child.session.signal_terminate()?;
        let deadline = std::time::Instant::now() + grace;
        loop {
            if let Some(code) = child.session.try_wait()? {
                return Ok(Some(code));
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.session.signal_kill()?;
        loop {
            if let Some(code) = child.session.try_wait()? {
                return Ok(Some(code));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Immediate SIGKILL of every live process group, then reap.
    ///
    /// Clears the interrupt-armed state.
    ///
    /// # Errors
    ///
    /// Propagates signal or wait errors. On non-Unix platforms with live
    /// children, returns [`io::ErrorKind::Unsupported`].
    pub fn kill_all(&mut self) -> io::Result<Vec<(String, i32)>> {
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
    ) -> io::Result<Option<Vec<(String, i32)>>> {
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
    ) -> io::Result<Vec<(String, i32)>> {
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
                child.session.signal_terminate()?;
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

    fn kill_remaining(&mut self) -> io::Result<Vec<(String, i32)>> {
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
                child.session.signal_kill()?;
            }

            let mut codes = Vec::with_capacity(self.children.len());
            // Take ownership so we can `wait` each session to completion.
            let sessions = std::mem::take(&mut self.children);
            for TrackedChild { id, session } in sessions {
                codes.push((id, session.wait()?));
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

    /// Wait briefly for init to reap killed grandchildren (zombie PGID race).
    #[cfg(unix)]
    fn assert_group_gone(pgid: u32) {
        for _ in 0..50 {
            if !group_alive(pgid) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("process group {pgid} still alive");
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_all_term_to_kill_many_children() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let sleep = unix_util("sleep");
        let child_count = 8usize;
        let mut pgids = Vec::with_capacity(child_count);

        for index in 0..child_count {
            let id = format!("sleep-{index}");
            let pgid = supervisor
                .spawn(&id, &sleep, &["30"], None, &env)
                .expect("spawn sleep");
            pgids.push(pgid);
        }
        assert_eq!(supervisor.len(), child_count);

        let codes = supervisor
            .shutdown_all(Duration::from_secs(3))
            .expect("shutdown_all");
        assert_eq!(codes.len(), child_count);
        assert!(supervisor.is_empty());
        for (_id, code) in codes {
            assert!(
                code == 128 + 15 || code == 128 + 9,
                "unexpected exit code {code}"
            );
        }
        for pgid in pgids {
            assert!(!group_alive(pgid), "group {pgid} still alive");
        }
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_all_stops_two_sleeps() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let sleep = unix_util("sleep");

        let pgid_a = supervisor
            .spawn("a", &sleep, &["30"], None, &env)
            .expect("spawn sleep a");
        let pgid_b = supervisor
            .spawn("b", &sleep, &["30"], None, &env)
            .expect("spawn sleep b");
        assert_eq!(supervisor.len(), 2);

        let codes = supervisor
            .shutdown_all(Duration::from_secs(2))
            .expect("shutdown_all");
        assert_eq!(codes.len(), 2);
        assert!(supervisor.is_empty());
        for (_id, code) in codes {
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
            .spawn("true", unix_util("true"), &[] as &[&str], None, &env)
            .expect("spawn true");
        supervisor
            .spawn("sleep", unix_util("sleep"), &["30"], None, &env)
            .expect("spawn sleep");

        let mut finished = None;
        for _ in 0..50 {
            if let Some((id, code)) = supervisor.try_wait_any().expect("try_wait_any") {
                finished = Some((id, code));
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(finished, Some(("true".to_owned(), 0)));
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

        // Ignore SIGTERM in the shell itself and spin in-process. Nested
        // `sleep` leaves a reparented zombie that can make killpg(0) succeed
        // briefly after the supervised bash is reaped (flaky on Linux CI).
        let ignore_term = ["-c", "trap '' TERM; while true; do true; done"];
        let pgid_a = supervisor
            .spawn("a", &bash, &ignore_term, None, &env)
            .expect("spawn a");
        let pgid_b = supervisor
            .spawn("b", &bash, &ignore_term, None, &env)
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
        for (_id, code) in codes {
            assert_eq!(code, 128 + 9, "expected SIGKILL exit, got {code}");
        }
        assert!(supervisor.is_empty());
        assert!(!group_alive(pgid_a));
        assert!(!group_alive(pgid_b));
    }

    #[cfg(unix)]
    #[test]
    fn handle_interrupt_first_strike_sends_sigterm_only() {
        let flags = InterruptFlags::install().expect("install flags");
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let sleep = unix_util("sleep");

        let pgid = supervisor
            .spawn("sleep", &sleep, &["30"], None, &env)
            .expect("spawn sleep");

        flags.trigger_for_test();
        let started = Instant::now();
        let codes = supervisor
            .handle_interrupt(&flags, Duration::from_secs(5))
            .expect("handle_interrupt")
            .expect("shutdown ran");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "first strike should not wait out the full grace window"
        );
        assert_eq!(codes.len(), 1);
        assert_eq!(
            codes[0].1,
            128 + 15,
            "first strike should SIGTERM, got {}",
            codes[0].1
        );
        assert!(supervisor.is_empty());
        assert!(!group_alive(pgid));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_all_escalates_when_child_ignores_sigterm() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let bash = unix_util("bash");
        let ignore_term = ["-c", "trap '' TERM; while true; do true; done"];

        let pgid = supervisor
            .spawn("stubborn", &bash, &ignore_term, None, &env)
            .expect("spawn");
        thread::sleep(Duration::from_millis(100));

        let started = Instant::now();
        let codes = supervisor
            .shutdown_all(Duration::from_millis(300))
            .expect("shutdown_all");
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "should wait for grace before SIGKILL"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "should not hang after grace"
        );
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].1, 128 + 9, "expected SIGKILL, got {}", codes[0].1);
        assert!(!group_alive(pgid));
    }

    #[cfg(unix)]
    #[test]
    fn nested_grandchildren_do_not_survive_shutdown() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let bash = unix_util("bash");
        let nested = ["-c", "bash -c 'exec sleep 60'"];
        let child_count = 4usize;
        let mut pgids = Vec::with_capacity(child_count);

        for index in 0..child_count {
            let id = format!("nested-{index}");
            let pgid = supervisor
                .spawn(&id, &bash, &nested, None, &env)
                .expect("spawn nested bash");
            pgids.push(pgid);
        }

        let codes = supervisor
            .shutdown_all(Duration::from_secs(2))
            .expect("shutdown_all");
        assert_eq!(codes.len(), child_count);
        assert!(supervisor.is_empty());
        for pgid in pgids {
            assert_group_gone(pgid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn kill_all_escalates_immediately() {
        let mut supervisor = Supervisor::new();
        let env = EnvironmentPolicy::Inherit;
        let bash = unix_util("bash");
        let ignore_term = ["-c", "trap '' TERM; while true; do true; done"];

        let pgid = supervisor
            .spawn("stubborn", &bash, &ignore_term, None, &env)
            .expect("spawn");
        thread::sleep(Duration::from_millis(100));

        let codes = supervisor.kill_all().expect("kill_all");
        assert_eq!(codes, vec![("stubborn".to_owned(), 128 + 9)]);
        assert!(!group_alive(pgid));
    }
}
