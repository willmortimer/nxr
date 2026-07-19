//! Watch generation restart orchestration (debounce).

use std::time::{Duration, Instant};

/// Coalesces dirty marks into a single ready signal after a quiet window.
#[derive(Clone, Debug)]
pub struct Debouncer {
    window: Duration,
    dirty_at: Option<Instant>,
}

impl Debouncer {
    /// Create a debouncer with the given quiet window.
    #[must_use]
    pub const fn new(window: Duration) -> Self {
        Self {
            window,
            dirty_at: None,
        }
    }

    /// Record that a filesystem change arrived (resets the quiet window).
    pub fn mark_dirty(&mut self) {
        self.dirty_at = Some(Instant::now());
    }

    /// Time remaining until a pending dirty mark becomes ready.
    #[must_use]
    pub fn time_until_ready(&self) -> Option<Duration> {
        let dirty_at = self.dirty_at?;
        let elapsed = dirty_at.elapsed();
        Some(self.window.saturating_sub(elapsed))
    }

    /// If the quiet window has elapsed since the last dirty mark, clear and
    /// return `true`.
    pub fn take_ready(&mut self) -> bool {
        let Some(dirty_at) = self.dirty_at else {
            return false;
        };
        if dirty_at.elapsed() < self.window {
            return false;
        }
        self.dirty_at = None;
        true
    }
}

/// Monotonic generation counter for watch restarts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Generation(u64);

impl Generation {
    /// Start at generation 0.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Current generation number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Advance to the next generation and return it.
    pub fn bump(&mut self) -> u64 {
        self.0 = self.0.saturating_add(1);
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn generation_bumps() {
        let mut generation = Generation::new();
        assert_eq!(generation.get(), 0);
        assert_eq!(generation.bump(), 1);
        assert_eq!(generation.bump(), 2);
    }

    #[test]
    fn debounce_not_ready_until_window() {
        let mut d = Debouncer::new(Duration::from_millis(40));
        assert!(!d.take_ready());
        d.mark_dirty();
        assert!(!d.take_ready());
        thread::sleep(Duration::from_millis(50));
        assert!(d.take_ready());
    }
}
