//! In-memory DAG watch state driven by task events or attach replay.

use std::collections::BTreeMap;

use nxr_core::sanitize::sanitize_terminal_text;
use nxr_task::{Event, NodeOutcome, OutputPayload};

/// Maximum decoded bytes retained per node stream in the TUI log tail.
const LOG_TAIL_CAPACITY: usize = 16 * 1024;

/// Lifecycle phase for one DAG node.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NodePhase {
    #[default]
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
    TimedOut,
}

impl NodePhase {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed out",
        }
    }

    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Queued => "○",
            Self::Running => "●",
            Self::Succeeded => "✓",
            Self::Failed => "✗",
            Self::Skipped => "⊘",
            Self::Cancelled => "⊗",
            Self::TimedOut => "⏱",
        }
    }
}

/// Per-node presentation state.
#[derive(Clone, Debug, Default)]
pub struct NodeState {
    pub phase: NodePhase,
    pub duration_ms: Option<u64>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// One-screen DAG watch model.
#[derive(Clone, Debug, Default)]
pub struct WatchState {
    pub run_id: Option<String>,
    pub root: String,
    pub node_order: Vec<String>,
    pub nodes: BTreeMap<String, NodeState>,
    pub selected: usize,
    pub run_complete: bool,
    pub success: Option<bool>,
    pub diagnostic: Option<String>,
}

impl WatchState {
    /// Apply one execution event.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::PlanCreated { root, run_id, .. } => {
                self.root = sanitize_terminal_text(root);
                self.run_id = run_id.clone();
            }
            Event::NodeQueued { node, .. } => {
                self.ensure_node(node);
                self.set_phase(node, NodePhase::Queued);
            }
            Event::NodeStarted { node, .. } => {
                self.ensure_node(node);
                self.set_phase(node, NodePhase::Running);
            }
            Event::StdoutChunk { node, payload } => {
                self.ensure_node(node);
                self.append_tail(node, true, payload);
            }
            Event::StderrChunk { node, payload } => {
                self.ensure_node(node);
                self.append_tail(node, false, payload);
            }
            Event::NodeExited {
                node,
                code,
                status,
                duration_ms,
                ..
            } => {
                self.ensure_node(node);
                let phase = node_phase_from_exit(*code, *status);
                self.set_phase(node, phase);
                if let Some(entry) = self.nodes.get_mut(node) {
                    entry.duration_ms = *duration_ms;
                }
            }
            Event::RunCompleted { success, run_id, .. } => {
                self.run_id = run_id.clone();
                self.run_complete = true;
                self.success = Some(*success);
            }
            Event::Diagnostic { message } => {
                self.diagnostic = Some(sanitize_terminal_text(message));
            }
        }
    }

    /// Selected node id, if any.
    #[must_use]
    pub fn selected_node(&self) -> Option<&str> {
        self.node_order.get(self.selected).map(String::as_str)
    }

    /// Move selection up/down, wrapping at ends.
    pub fn move_selection(&mut self, delta: isize) {
        if self.node_order.is_empty() {
            return;
        }
        let len = isize::try_from(self.node_order.len()).unwrap_or(isize::MAX);
        let current = isize::try_from(self.selected).unwrap_or(0);
        let next = (current + delta).rem_euclid(len);
        self.selected = usize::try_from(next).unwrap_or(0);
    }

    fn ensure_node(&mut self, node: &str) {
        let safe = sanitize_terminal_text(node);
        if !self.nodes.contains_key(&safe) {
            self.node_order.push(safe.clone());
            self.nodes.insert(safe, NodeState::default());
        }
    }

    fn set_phase(&mut self, node: &str, phase: NodePhase) {
        let safe = sanitize_terminal_text(node);
        if let Some(entry) = self.nodes.get_mut(&safe) {
            entry.phase = phase;
        }
    }

    fn append_tail(&mut self, node: &str, stdout: bool, payload: &OutputPayload) {
        let safe = sanitize_terminal_text(node);
        let Some(entry) = self.nodes.get_mut(&safe) else {
            return;
        };
        let text = decode_payload(payload);
        let tail = if stdout {
            &mut entry.stdout_tail
        } else {
            &mut entry.stderr_tail
        };
        tail.push_str(&text);
        trim_tail(tail);
    }
}

fn node_phase_from_exit(code: Option<i32>, status: Option<NodeOutcome>) -> NodePhase {
    match status {
        Some(NodeOutcome::Succeeded) => NodePhase::Succeeded,
        Some(NodeOutcome::Failed) => NodePhase::Failed,
        Some(NodeOutcome::Cancelled) => NodePhase::Cancelled,
        Some(NodeOutcome::Skipped) => NodePhase::Skipped,
        Some(NodeOutcome::TimedOut) => NodePhase::TimedOut,
        None => {
            if matches!(code, Some(0)) {
                NodePhase::Succeeded
            } else {
                NodePhase::Failed
            }
        }
    }
}

fn decode_payload(payload: &OutputPayload) -> String {
    let bytes = payload.as_bytes();
    let text = String::from_utf8_lossy(bytes);
    sanitize_terminal_text(&text)
}

fn trim_tail(tail: &mut String) {
    if tail.len() <= LOG_TAIL_CAPACITY {
        return;
    }
    let drop = tail.len() - LOG_TAIL_CAPACITY;
    let boundary = tail
        .char_indices()
        .find(|(index, _)| *index >= drop)
        .map_or(drop, |(index, _)| index);
    tail.drain(..boundary);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::OutputPayload;

    #[test]
    fn plan_created_sets_root_and_run_id() {
        let mut state = WatchState::default();
        state.apply(&Event::PlanCreated {
            root: "ci".to_owned(),
            roots: None,
            node_count: 2,
            run_id: Some("run-deadbeef".to_owned()),
        });
        assert_eq!(state.root, "ci");
        assert_eq!(state.run_id.as_deref(), Some("run-deadbeef"));
    }

    #[test]
    fn node_lifecycle_updates_phases() {
        let mut state = WatchState::default();
        state.apply(&Event::node_queued("fmt"));
        state.apply(&Event::node_started("fmt"));
        state.apply(&Event::node_exited("fmt", Some(0)));
        let node = state.nodes.get("fmt").expect("node");
        assert_eq!(node.phase, NodePhase::Succeeded);
    }

    #[test]
    fn stdout_chunks_append_to_selected_tail() {
        let mut state = WatchState::default();
        state.apply(&Event::StdoutChunk {
            node: "api".to_owned(),
            payload: OutputPayload::utf8("hello\n"),
        });
        let node = state.nodes.get("api").expect("node");
        assert_eq!(node.stdout_tail, "hello\n");
    }

    #[test]
    fn selection_wraps() {
        let mut state = WatchState::default();
        state.apply(&Event::node_queued("a"));
        state.apply(&Event::node_queued("b"));
        state.selected = 1;
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.move_selection(-1);
        assert_eq!(state.selected, 1);
    }
}
