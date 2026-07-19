//! Typed execution events and a synchronous sink trait.
//!
//! Renderers and schedulers share this bus so scheduling stays decoupled from
//! presentation. Events are sync-only (no Tokio).

use serde::{Deserialize, Serialize};

/// Typed events emitted during plan construction and node execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An immutable execution plan was produced.
    PlanCreated {
        /// Root task id the plan was built for.
        root: String,
        /// Number of nodes in the plan.
        node_count: usize,
    },
    /// A node entered the ready/queued set.
    NodeQueued {
        /// Task id.
        node: String,
    },
    /// A node process (or equivalent) started.
    NodeStarted {
        /// Task id.
        node: String,
    },
    /// A chunk of stdout from a running node.
    StdoutChunk {
        /// Task id.
        node: String,
        /// UTF-8 text (lossy conversion is the emitter's responsibility).
        text: String,
    },
    /// A chunk of stderr from a running node.
    StderrChunk {
        /// Task id.
        node: String,
        /// UTF-8 text (lossy conversion is the emitter's responsibility).
        text: String,
    },
    /// A node finished with an exit status.
    NodeExited {
        /// Task id.
        node: String,
        /// Process exit code (`None` when terminated by signal / unavailable).
        code: Option<i32>,
    },
    /// The overall run finished.
    RunCompleted {
        /// Whether every required node succeeded under the active failure policy.
        success: bool,
    },
    /// Non-fatal diagnostic for operators / renderers.
    Diagnostic {
        /// Human-readable message (already sanitized for terminals by the emitter).
        message: String,
    },
}

/// Synchronous consumer of [`Event`] values.
pub trait EventSink {
    /// Receive one event.
    fn emit(&mut self, event: Event);
}

/// Sink that records every event in order (useful in tests and dry-runs).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordingSink {
    events: Vec<Event>,
}

impl RecordingSink {
    /// Create an empty recording sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow recorded events in emission order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Take ownership of recorded events, leaving the sink empty.
    #[must_use]
    pub fn into_events(self) -> Vec<Event> {
        self.events
    }

    /// Clear recorded events.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
}

/// No-op sink for callers that discard events.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&mut self, _event: Event) {}
}

/// Exhaustively classify an event for diagnostics / tests.
///
/// Returning a static label forces new variants to be handled at compile time.
#[must_use]
pub fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::PlanCreated { .. } => "plan_created",
        Event::NodeQueued { .. } => "node_queued",
        Event::NodeStarted { .. } => "node_started",
        Event::StdoutChunk { .. } => "stdout_chunk",
        Event::StderrChunk { .. } => "stderr_chunk",
        Event::NodeExited { .. } => "node_exited",
        Event::RunCompleted { .. } => "run_completed",
        Event::Diagnostic { .. } => "diagnostic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_sink_preserves_order() {
        let mut sink = RecordingSink::new();
        sink.emit(Event::PlanCreated {
            root: "ci".to_owned(),
            node_count: 2,
        });
        sink.emit(Event::NodeQueued {
            node: "fmt".to_owned(),
        });
        sink.emit(Event::NodeStarted {
            node: "fmt".to_owned(),
        });
        sink.emit(Event::StdoutChunk {
            node: "fmt".to_owned(),
            text: "ok\n".to_owned(),
        });
        sink.emit(Event::StderrChunk {
            node: "fmt".to_owned(),
            text: String::new(),
        });
        sink.emit(Event::NodeExited {
            node: "fmt".to_owned(),
            code: Some(0),
        });
        sink.emit(Event::RunCompleted { success: true });
        sink.emit(Event::Diagnostic {
            message: "done".to_owned(),
        });

        assert_eq!(sink.events().len(), 8);
        assert_eq!(event_kind(&sink.events()[0]), "plan_created");
        assert_eq!(event_kind(&sink.events()[7]), "diagnostic");
    }

    #[test]
    fn event_json_round_trip() {
        let events = vec![
            Event::PlanCreated {
                root: "d".to_owned(),
                node_count: 4,
            },
            Event::NodeQueued {
                node: "a".to_owned(),
            },
            Event::NodeStarted {
                node: "a".to_owned(),
            },
            Event::StdoutChunk {
                node: "a".to_owned(),
                text: "hello".to_owned(),
            },
            Event::StderrChunk {
                node: "a".to_owned(),
                text: "warn".to_owned(),
            },
            Event::NodeExited {
                node: "a".to_owned(),
                code: Some(1),
            },
            Event::NodeExited {
                node: "b".to_owned(),
                code: None,
            },
            Event::RunCompleted { success: false },
            Event::Diagnostic {
                message: "cycle avoided".to_owned(),
            },
        ];

        for event in events {
            let encoded = serde_json::to_value(&event).expect("serialize");
            let decoded: Event = serde_json::from_value(encoded).expect("deserialize");
            assert_eq!(decoded, event);
            // Touch every variant via the exhaustive classifier.
            assert!(!event_kind(&decoded).is_empty());
        }
    }

    #[test]
    fn null_sink_discards() {
        let mut sink = NullSink;
        sink.emit(Event::Diagnostic {
            message: "ignored".to_owned(),
        });
    }
}
