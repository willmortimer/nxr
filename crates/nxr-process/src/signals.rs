//! Signal forwarding and exit-status mapping.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nxr_core::diagnostics::exit;

/// Map a child wait status to a process exit code.
///
/// Prefer the child's normal exit code when present. On Unix, termination by
/// signal becomes `128 + signal` (shell convention). If neither is available,
/// returns [`exit::CHILD_FAILED`].
#[must_use]
pub fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }

    exit::CHILD_FAILED
}

/// Flags set by SIGINT/SIGTERM handlers for cooperative shutdown.
#[derive(Debug)]
pub struct InterruptFlags {
    got_int: Arc<AtomicBool>,
    got_term: Arc<AtomicBool>,
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}

impl InterruptFlags {
    /// Install SIGINT/SIGTERM handlers that set interrupt flags.
    ///
    /// # Errors
    ///
    /// Returns an error when handlers cannot be registered.
    pub fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use signal_hook::consts::signal::{SIGINT, SIGTERM};
            use signal_hook::flag;

            let got_int = Arc::new(AtomicBool::new(false));
            let got_term = Arc::new(AtomicBool::new(false));
            let int_id =
                flag::register(SIGINT, Arc::clone(&got_int)).map_err(std::io::Error::other)?;
            let term_id =
                flag::register(SIGTERM, Arc::clone(&got_term)).map_err(std::io::Error::other)?;
            Ok(Self {
                got_int,
                got_term,
                registrations: vec![int_id, term_id],
            })
        }

        #[cfg(not(unix))]
        {
            Ok(Self {
                got_int: Arc::new(AtomicBool::new(false)),
                got_term: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    /// Whether SIGINT or SIGTERM has been received since the last check.
    #[must_use]
    pub fn take_pending(&self) -> bool {
        let int = self.got_int.swap(false, Ordering::SeqCst);
        let term = self.got_term.swap(false, Ordering::SeqCst);
        int || term
    }

    /// Whether an interrupt is currently flagged (does not clear).
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.got_int.load(Ordering::SeqCst) || self.got_term.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for InterruptFlags {
    fn drop(&mut self) {
        use signal_hook::low_level;
        for id in self.registrations.drain(..) {
            let _ = low_level::unregister(id);
        }
    }
}

#[cfg(unix)]
pub(crate) mod unix {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    use signal_hook::consts::signal::{SIGINT, SIGTERM};
    use signal_hook::flag;
    use signal_hook::low_level;

    /// Installs SIGINT/SIGTERM handlers that set flags for later forwarding.
    ///
    /// Handlers only flip atomics (async-signal-safe). [`Self::poll_and_forward`]
    /// sends the matching signal to the child's process group from normal code.
    pub(crate) struct SignalForwarder {
        got_int: Arc<AtomicBool>,
        got_term: Arc<AtomicBool>,
        registrations: Vec<signal_hook::SigId>,
    }

    impl SignalForwarder {
        pub(crate) fn install() -> io::Result<Self> {
            let got_int = Arc::new(AtomicBool::new(false));
            let got_term = Arc::new(AtomicBool::new(false));

            let int_id = flag::register(SIGINT, Arc::clone(&got_int)).map_err(io::Error::other)?;
            let term_id =
                flag::register(SIGTERM, Arc::clone(&got_term)).map_err(io::Error::other)?;

            Ok(Self {
                got_int,
                got_term,
                registrations: vec![int_id, term_id],
            })
        }

        /// If SIGINT/SIGTERM arrived, forward them to `pgid`.
        pub(crate) fn poll_and_forward(&self, pgid: u32) {
            // Process group leaders use pgid == pid after `process_group(0)`.
            let group = Pid::from_raw(i32::try_from(pgid).unwrap_or(0));
            if group.as_raw() <= 0 {
                return;
            }

            if self.got_int.swap(false, Ordering::SeqCst) {
                let _ = killpg(group, Signal::SIGINT);
            }
            if self.got_term.swap(false, Ordering::SeqCst) {
                let _ = killpg(group, Signal::SIGTERM);
            }
        }
    }

    impl Drop for SignalForwarder {
        fn drop(&mut self) {
            for id in self.registrations.drain(..) {
                let _ = low_level::unregister(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::exit_code_from_status;

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
    fn exit_code_passthrough_for_normal_exit() {
        let status = std::process::Command::new(unix_util("true"))
            .status()
            .expect("spawn true");
        assert_eq!(exit_code_from_status(status), 0);

        let status = std::process::Command::new(unix_util("false"))
            .status()
            .expect("spawn false");
        assert_eq!(exit_code_from_status(status), 1);
    }

    #[cfg(unix)]
    #[test]
    fn signal_death_maps_to_128_plus_signal() {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        let mut child = Command::new(unix_util("sleep"))
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pgid = Pid::from_raw(i32::try_from(child.id()).expect("pid fits i32"));
        killpg(pgid, Signal::SIGTERM).expect("killpg SIGTERM");
        let status = child.wait().expect("wait for sleep");
        assert_eq!(exit_code_from_status(status), 128 + Signal::SIGTERM as i32);
    }
}
