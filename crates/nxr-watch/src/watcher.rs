//! Filesystem watch sessions with debounce for nxr generations.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

use crate::restart::Debouncer;

/// Default debounce window for coalescing filesystem events.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// Configuration for a watch session over a flake root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchConfig {
    /// Directory tree to watch (typically the flake root).
    pub root: Utf8PathBuf,
    /// Coalesce window before a restart is requested.
    pub debounce: Duration,
}

impl WatchConfig {
    /// Watch `root` with the default debounce window.
    #[must_use]
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        Self {
            root: root.into(),
            debounce: DEFAULT_DEBOUNCE,
        }
    }
}

/// Errors while creating or running a watch session.
#[derive(Debug, Error)]
pub enum WatchError {
    /// Underlying notify/watcher failure.
    #[error("filesystem watch error: {0}")]
    Notify(#[from] notify::Error),

    /// Channel disconnected while waiting for events.
    #[error("watch event channel disconnected")]
    Disconnected,
}

/// Outcome of waiting for the next restart signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchPoll {
    /// Debounced filesystem change — start a new generation.
    Restart,
    /// Wait timed out with no pending restart.
    Timeout,
}

/// Active recursive watch over a project root.
pub struct WatchSession {
    _watcher: RecommendedWatcher,
    events: Receiver<()>,
    debouncer: Debouncer,
}

impl WatchSession {
    /// Start watching `config.root` recursively.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::Notify`] when the OS watcher cannot be created or
    /// the root cannot be watched.
    pub fn start(config: &WatchConfig) -> Result<Self, WatchError> {
        let (tx, rx) = mpsc::channel();
        let root = config.root.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
                let Ok(event) = result else {
                    return;
                };
                if !is_interesting_event(event.kind) {
                    return;
                }
                if event
                    .paths
                    .iter()
                    .any(|path| should_ignore_path(&root, path))
                {
                    return;
                }
                let _ = tx.send(());
            })?;

        watcher.watch(config.root.as_std_path(), RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            events: rx,
            debouncer: Debouncer::new(config.debounce),
        })
    }

    /// Drain pending FS events into the debouncer (non-blocking).
    pub fn drain_events(&mut self) {
        while self.events.try_recv().is_ok() {
            self.debouncer.mark_dirty();
        }
    }

    /// Wait up to `timeout` for a debounced restart request.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::Disconnected`] if the watcher channel closes.
    pub fn poll_restart(&mut self, timeout: Duration) -> Result<WatchPoll, WatchError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_events();
            if self.debouncer.take_ready() {
                return Ok(WatchPoll::Restart);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(WatchPoll::Timeout);
            }

            // Wake early once debounce window elapses after a dirty mark.
            let wait = self
                .debouncer
                .time_until_ready()
                .map_or(remaining, |until| remaining.min(until));

            match self.events.recv_timeout(wait) {
                Ok(()) => {
                    self.debouncer.mark_dirty();
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.drain_events();
                    if self.debouncer.take_ready() {
                        return Ok(WatchPoll::Restart);
                    }
                    if Instant::now() >= deadline {
                        return Ok(WatchPoll::Timeout);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(WatchError::Disconnected),
            }
        }
    }
}

fn is_interesting_event(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

/// Whether `path` should be ignored relative to the watch root.
#[must_use]
pub fn should_ignore_path(root: &Utf8Path, path: &Path) -> bool {
    let Some(path) = Utf8Path::from_path(path) else {
        return true;
    };

    // Never watch Nix store paths.
    if path.starts_with("/nix/store") {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| {
        let name = component.as_str();
        name == ".git" || name == "target" || name == "result" || name.starts_with("result-")
    })
}

/// Test helper: expose ignore logic with owned paths.
#[cfg(test)]
pub(crate) fn ignore_check(root: &str, path: &str) -> bool {
    should_ignore_path(Utf8Path::new(root), Path::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn ignores_git_target_and_result() {
        let root = "/proj";
        assert!(ignore_check(root, "/proj/.git/HEAD"));
        assert!(ignore_check(root, "/proj/target/debug/nxr"));
        assert!(ignore_check(root, "/proj/result"));
        assert!(ignore_check(root, "/proj/result-1"));
        assert!(ignore_check(root, "/nix/store/abc/bin/hello"));
        assert!(!ignore_check(root, "/proj/src/main.rs"));
    }

    #[test]
    fn debounce_coalesces_rapid_marks() {
        let mut debouncer = Debouncer::new(Duration::from_millis(50));
        debouncer.mark_dirty();
        thread::sleep(Duration::from_millis(10));
        debouncer.mark_dirty();
        assert!(!debouncer.take_ready());
        thread::sleep(Duration::from_millis(60));
        assert!(debouncer.take_ready());
        assert!(!debouncer.take_ready());
    }

    #[test]
    fn watch_session_sees_file_create() {
        let dir = tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let mut session = WatchSession::start(&WatchConfig {
            root: root.clone(),
            debounce: Duration::from_millis(50),
        })
        .expect("start watch");

        let file = root.join("trigger.txt");
        fs::write(file.as_std_path(), b"hi").expect("write");

        let mut saw = false;
        for _ in 0..40 {
            match session
                .poll_restart(Duration::from_millis(50))
                .expect("poll")
            {
                WatchPoll::Restart => {
                    saw = true;
                    break;
                }
                WatchPoll::Timeout => {}
            }
        }
        assert!(saw, "expected restart after file create");
    }
}
