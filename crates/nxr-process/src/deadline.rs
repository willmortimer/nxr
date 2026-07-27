//! Min-deadline queue for supervision loops.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::time::{Duration, Instant};

/// Tracks the nearest upcoming deadlines without scanning every live node each poll.
#[derive(Debug, Default)]
pub struct DeadlineQueue {
    heap: BinaryHeap<Reverse<(Instant, u32)>>,
    cancelled: HashSet<u32>,
}

impl DeadlineQueue {
    /// Create an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule `node` to fire at `deadline`.
    pub fn insert(&mut self, node: u32, deadline: Instant) {
        self.cancelled.remove(&node);
        self.heap.push(Reverse((deadline, node)));
    }

    /// Drop pending deadlines for `node` (exit, timeout, or cancellation).
    pub fn cancel(&mut self, node: u32) {
        self.cancelled.insert(node);
    }

    /// Remove every node whose deadline is at or before `now`.
    pub fn pop_expired(&mut self, now: Instant) -> Vec<u32> {
        let mut expired = Vec::new();
        while let Some(&Reverse((deadline, _))) = self.heap.peek() {
            if deadline > now {
                break;
            }
            let Some(Reverse((_, node))) = self.heap.pop() else {
                break;
            };
            if self.cancelled.remove(&node) {
                continue;
            }
            expired.push(node);
        }
        expired
    }

    /// Time until the next pending deadline, if any.
    #[must_use]
    pub fn time_until_next(&self, now: Instant) -> Option<Duration> {
        self.heap
            .iter()
            .filter(|Reverse((_, node))| !self.cancelled.contains(node))
            .map(|Reverse((deadline, _))| deadline.saturating_duration_since(now))
            .min()
    }

    /// Whether there are no live deadlines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::DeadlineQueue;
    use std::time::{Duration, Instant};

    #[test]
    fn pop_expired_returns_nodes_in_deadline_order() {
        let mut queue = DeadlineQueue::new();
        let now = Instant::now();
        queue.insert(1, now + Duration::from_millis(30));
        queue.insert(2, now + Duration::from_millis(10));
        queue.insert(3, now + Duration::from_millis(20));

        assert!(queue.pop_expired(now).is_empty());
        assert_eq!(queue.pop_expired(now + Duration::from_millis(15)), vec![2]);
        assert_eq!(queue.pop_expired(now + Duration::from_millis(25)), vec![3]);
        assert_eq!(queue.pop_expired(now + Duration::from_millis(35)), vec![1]);
        assert!(
            queue
                .pop_expired(now + Duration::from_millis(35))
                .is_empty()
        );
    }

    #[test]
    fn cancel_suppresses_future_pops() {
        let mut queue = DeadlineQueue::new();
        let now = Instant::now();
        queue.insert(7, now + Duration::from_millis(5));
        queue.cancel(7);
        assert!(queue.pop_expired(now + Duration::from_secs(1)).is_empty());
    }
}
