//! OSC 52 failure clipboard integration for task runs.

use nxr_core::{FailureLine, maybe_emit_failure_clipboard};
use nxr_task::{Event, EventSink, NodeOutcome};

/// Collect failed node lines and expose them for post-run clipboard emission.
pub struct Osc52Collector<S> {
    inner: S,
    failures: Vec<FailureLine>,
}

impl<S> Osc52Collector<S> {
    /// Wrap `inner` and record failure lines from task events.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            failures: Vec::new(),
        }
    }

    /// Borrow collected failure lines (node names + compact status).
    #[must_use]
    pub fn failures(&self) -> &[FailureLine] {
        &self.failures
    }

    /// Borrow the wrapped sink.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Emit OSC 52 when `exit_code` indicates a failed run.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when writing to stderr fails.
    pub fn maybe_emit_on_exit(&self, exit_code: i32) -> std::io::Result<()> {
        maybe_emit_failure_clipboard(self.failures(), exit_code)
    }
}

impl<S: EventSink> EventSink for Osc52Collector<S> {
    fn emit(&mut self, event: Event) {
        if let Event::NodeExited {
            node, code, status, ..
        } = &event
            && let Some(line) = failure_line(node, *code, *status)
        {
            self.failures.push(line);
        }
        self.inner.emit(event);
    }
}

fn failure_line(node: &str, code: Option<i32>, status: Option<NodeOutcome>) -> Option<FailureLine> {
    match status {
        Some(NodeOutcome::Failed) => Some(FailureLine::exit(
            node,
            code.unwrap_or(nxr_core::diagnostics::exit::CHILD_FAILED),
        )),
        Some(NodeOutcome::TimedOut) => Some(FailureLine::status(node, "timed_out")),
        Some(NodeOutcome::Cancelled) => Some(FailureLine::status(node, "cancelled")),
        Some(NodeOutcome::Skipped) => Some(FailureLine::status(node, "skipped")),
        Some(NodeOutcome::Succeeded) => None,
        None => match code {
            Some(0) | None => None,
            Some(exit_code) => Some(FailureLine::exit(node, exit_code)),
        },
    }
}

/// Emit OSC 52 for a failed bare app or workspace script run (best-effort).
pub fn maybe_emit_app_failure_clipboard(name: &str, exit_code: i32) {
    if exit_code == 0 {
        return;
    }
    let _ = maybe_emit_failure_clipboard(&[FailureLine::exit(name, exit_code)], exit_code);
}

#[cfg(test)]
mod tests {
    use super::{Osc52Collector, failure_line};
    use nxr_task::{Event, EventSink, NodeOutcome, RecordingSink};

    #[test]
    fn collector_records_failed_and_derived_nodes() {
        let mut sink = Osc52Collector::new(RecordingSink::new());
        sink.emit(Event::node_exited("lint", Some(1)));
        sink.emit(Event::NodeExited {
            node: "gate".to_owned(),
            code: None,
            status: Some(NodeOutcome::Cancelled),
            duration_ms: None,
            started_at: None,
            finished_at: None,
            reason: Some("fail_fast".to_owned()),
            seq: None,
        });
        sink.emit(Event::node_exited("ok", Some(0)));

        assert_eq!(sink.failures().len(), 2);
        assert_eq!(sink.failures()[0].name, "lint");
        assert_eq!(sink.failures()[0].detail, "exit 1");
        assert_eq!(sink.failures()[1].name, "gate");
        assert_eq!(sink.failures()[1].detail, "cancelled");
    }

    #[test]
    fn failure_line_maps_outcomes() {
        assert_eq!(
            failure_line("a", Some(2), Some(NodeOutcome::Failed))
                .expect("failed")
                .detail,
            "exit 2"
        );
        assert_eq!(
            failure_line("b", None, Some(NodeOutcome::TimedOut))
                .expect("timed out")
                .detail,
            "timed_out"
        );
        assert!(failure_line("c", Some(0), None).is_none());
    }
}
