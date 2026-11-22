//! Deterministic read-side projection of authoritative execution evidence.
//!
//! Child log bytes are not deserialized as events and cannot complete a run.
//! The scheduler, supervisor and domain verifiers retain their authority.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::artifact_ref::{ArtifactRef, VerifiedArtifactRef};
use crate::events::{Event, NodeOutcome, RunOutcome};

pub const RUN_VIEW_V1: &str = "nxr-run-view-v1";
pub const RUN_PLAN_V1: &str = "nxr-run-plan-v1";
pub const MAX_NODE_ARTIFACTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPlanMetadata {
    pub schema_version: String,
    pub run_id: String,
    pub operation_id: String,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub workspace_id: String,
    /// Public metadata only: node -> parameter name -> origin, never values.
    #[serde(default)]
    pub input_provenance: BTreeMap<String, BTreeMap<String, crate::typed_inputs::InputProvenance>>,
    pub roots: Vec<String>,
    pub dependencies: BTreeMap<String, Vec<String>>,
    pub logs_captured: bool,
    #[serde(default)]
    pub uncaptured_nodes: Vec<String>,
}

impl RunPlanMetadata {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RUN_PLAN_V1 || self.run_id.is_empty() {
            return Err("invalid run plan identity or schema".into());
        }
        if self.dependencies.len() > 100_000 {
            return Err("run plan exceeds the node limit".into());
        }
        for root in &self.roots {
            if !self.dependencies.contains_key(root) {
                return Err(format!("root {root} is absent from the plan"));
            }
        }
        for (node, dependencies) in &self.dependencies {
            if node.is_empty()
                || dependencies
                    .iter()
                    .any(|dependency| !self.dependencies.contains_key(dependency))
            {
                return Err(format!("invalid dependencies for {node}"));
            }
        }
        if self
            .uncaptured_nodes
            .iter()
            .any(|node| !self.dependencies.contains_key(node))
            || self
                .input_provenance
                .keys()
                .any(|node| !self.dependencies.contains_key(node))
        {
            return Err("run capture or input metadata names a node outside its plan".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeAttemptView {
    pub ordinal: u64,
    pub attempt_id: Option<String>,
    pub location: String,
    pub started_sequence: u64,
    pub terminal_sequence: Option<u64>,
    pub outcome: Option<NodeOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeView {
    pub dependencies: Vec<String>,
    pub outcome: Option<NodeOutcome>,
    pub attempts: Vec<NodeAttemptView>,
    pub phase: Option<String>,
    pub reason: Option<String>,
    pub duration_ms: Option<u64>,
    pub reused: bool,
    pub detached: bool,
    pub terminal_sequence: Option<u64>,
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub artifacts_truncated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CausalSummary {
    pub first_failure: Option<String>,
    pub independent_failures: Vec<String>,
    pub dependent_skips: BTreeMap<String, Vec<String>>,
    pub cancelled: Vec<String>,
    pub timed_out: Vec<String>,
    pub artifact_failures: BTreeMap<String, String>,
    pub uncertainties: Vec<String>,
    /// Nodes whose terminal execution evidence is still outstanding.
    pub outstanding_nodes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub schema_version: String,
    pub plan: RunPlanMetadata,
    pub state: RunState,
    pub last_sequence: u64,
    pub nodes: BTreeMap<String, NodeView>,
    pub summary: CausalSummary,
    pub diagnostics: Vec<crate::run_diagnostics::Diagnostic>,
    pub diagnostics_truncated: bool,
    pub terminal_receipt_observed: bool,
    /// The authority's reported terminal result, kept separate from the
    /// projection's current completeness and artifact verification state.
    pub reported_outcome: Option<RunOutcome>,
}

impl RunView {
    pub fn new(plan: RunPlanMetadata) -> Result<Self, String> {
        plan.validate()?;
        let nodes = plan
            .dependencies
            .iter()
            .map(|(node, dependencies)| {
                (
                    node.clone(),
                    NodeView {
                        dependencies: dependencies.clone(),
                        outcome: None,
                        attempts: Vec::new(),
                        phase: None,
                        reason: None,
                        duration_ms: None,
                        reused: false,
                        detached: false,
                        terminal_sequence: None,
                        artifacts: Vec::new(),
                        artifacts_truncated: false,
                    },
                )
            })
            .collect();
        Ok(Self {
            schema_version: RUN_VIEW_V1.into(),
            plan,
            state: RunState::Pending,
            last_sequence: 0,
            nodes,
            summary: CausalSummary::default(),
            diagnostics: Vec::new(),
            diagnostics_truncated: false,
            terminal_receipt_observed: false,
            reported_outcome: None,
        })
    }

    /// Apply only execution events supplied by the authoritative recorder.
    pub fn apply(&mut self, sequence: u64, event: &Event) -> Result<(), String> {
        self.check_sequence(sequence)?;
        if let Event::ArtifactReused { run_id, .. }
        | Event::ClientDetached { run_id, .. }
        | Event::Phase { run_id, .. } = event
            && run_id != &self.plan.run_id
        {
            return Err("execution evidence belongs to another run".into());
        }
        match event {
            Event::PlanCreated { run_id, .. } => {
                if run_id.as_ref().is_some_and(|id| id != &self.plan.run_id) {
                    return Err("event run identity does not match its plan".into());
                }
            }
            Event::NodeQueued { node, .. } => {
                self.node(node)?;
            }
            Event::NodeStarted { node, .. } => {
                let entry = self.node_mut(node)?;
                entry.outcome = None;
                entry.detached = false;
                entry.reused = false;
                entry.terminal_sequence = None;
                entry.attempts.push(NodeAttemptView {
                    ordinal: entry.attempts.len() as u64 + 1,
                    attempt_id: None,
                    location: "local".into(),
                    started_sequence: sequence,
                    terminal_sequence: None,
                    outcome: None,
                });
                self.state = RunState::Running;
            }
            Event::StdoutChunk { node, .. } | Event::StderrChunk { node, .. } => {
                self.node(node)?;
            }
            Event::NodeExited {
                node,
                code,
                status,
                duration_ms,
                reason,
                ..
            } => {
                let outcome = status.unwrap_or(if *code == Some(0) {
                    NodeOutcome::Succeeded
                } else {
                    NodeOutcome::Failed
                });
                if outcome == NodeOutcome::Succeeded && *code != Some(0) {
                    return Err("successful node outcome lacks a successful exit code".into());
                }
                let entry = self.node_mut(node)?;
                entry.outcome = Some(outcome);
                entry.duration_ms = *duration_ms;
                entry.reason.clone_from(reason);
                entry.terminal_sequence = Some(sequence);
                entry.detached = false;
                if let Some(attempt) = entry.attempts.last_mut() {
                    attempt.outcome = Some(outcome);
                    attempt.terminal_sequence = Some(sequence);
                }
            }
            Event::ArtifactReused { node, .. } => {
                let entry = self.node_mut(node)?;
                entry.reused = true;
                entry.outcome = Some(NodeOutcome::Succeeded);
                entry.terminal_sequence = Some(sequence);
            }
            Event::ClientDetached {
                node, attempt_id, ..
            } => {
                let entry = self.node_mut(node)?;
                entry.detached = true;
                if let Some(attempt) = entry.attempts.last_mut() {
                    attempt.attempt_id = Some(attempt_id.clone());
                    attempt.location = "remote".into();
                }
            }
            Event::Phase {
                node,
                attempt_id,
                phase,
                source,
                ..
            } => {
                let entry = self.node_mut(node)?;
                entry.phase = Some(phase.clone());
                if let Some(attempt) = entry.attempts.last_mut() {
                    if attempt_id.is_some() {
                        attempt.attempt_id.clone_from(attempt_id);
                    }
                    if source.starts_with("remote") {
                        attempt.location = "remote".into();
                    }
                }
            }
            Event::Diagnostic { .. } => {}
            Event::RunCompleted {
                success,
                status,
                run_id,
                ..
            } => {
                if run_id.as_ref().is_some_and(|id| id != &self.plan.run_id) {
                    return Err("terminal run identity does not match its plan".into());
                }
                if status.is_some_and(|outcome| (outcome == RunOutcome::Succeeded) != *success) {
                    return Err("run terminal success disagrees with its structured outcome".into());
                }
                self.terminal_receipt_observed = true;
                self.reported_outcome = Some(status.unwrap_or(if *success {
                    RunOutcome::Succeeded
                } else {
                    RunOutcome::Failed
                }));
            }
        }
        self.last_sequence = sequence;
        self.refresh_summary();
        Ok(())
    }

    /// Advance over separately stored logs without interpreting their bytes.
    pub fn apply_log(&mut self, sequence: u64, node: &str) -> Result<(), String> {
        self.check_sequence(sequence)?;
        self.node(node)?;
        self.last_sequence = sequence;
        Ok(())
    }

    pub fn declare_artifact(
        &mut self,
        sequence: u64,
        node: &str,
        artifact: &ArtifactRef,
    ) -> Result<(), String> {
        self.check_sequence(sequence)?;
        artifact.validate()?;
        let mut artifact = artifact.clone();
        artifact.verified = false;
        self.insert_artifact(node, artifact)?;
        self.last_sequence = sequence;
        Ok(())
    }

    pub fn publish_verified_artifact(
        &mut self,
        sequence: u64,
        node: &str,
        artifact: &VerifiedArtifactRef,
    ) -> Result<(), String> {
        self.apply_verified_record(sequence, node, artifact.reference())
    }

    /// Only the trusted journal reader uses persisted verifier records.
    pub(crate) fn apply_verified_record(
        &mut self,
        sequence: u64,
        node: &str,
        artifact: &ArtifactRef,
    ) -> Result<(), String> {
        self.check_sequence(sequence)?;
        artifact.validate()?;
        if !artifact.verified {
            return Err("verified artifact event has no verification evidence".into());
        }
        self.insert_artifact(node, artifact.clone())?;
        self.last_sequence = sequence;
        Ok(())
    }

    pub fn artifact_failure(
        &mut self,
        sequence: u64,
        node: &str,
        message: &str,
    ) -> Result<(), String> {
        self.check_sequence(sequence)?;
        self.node(node)?;
        self.summary
            .artifact_failures
            .insert(node.into(), message.chars().take(1024).collect());
        self.last_sequence = sequence;
        self.refresh_summary();
        Ok(())
    }

    pub fn uncertainty(&mut self, sequence: u64, message: &str) -> Result<(), String> {
        self.check_sequence(sequence)?;
        self.add_uncertainty(message.chars().take(1024).collect());
        self.last_sequence = sequence;
        self.refresh_summary();
        Ok(())
    }

    fn insert_artifact(&mut self, node: &str, artifact: ArtifactRef) -> Result<(), String> {
        let node = self.node_mut(node)?;
        if let Some(existing) = node
            .artifacts
            .iter_mut()
            .find(|item| item.name == artifact.name && item.digest == artifact.digest)
        {
            if artifact.verified {
                *existing = artifact;
            }
        } else if node.artifacts.len() < MAX_NODE_ARTIFACTS {
            node.artifacts.push(artifact);
        } else {
            // Projection retention is not an execution/verification failure.
            // Every declaration remains available in the paged artifact index.
            node.artifacts_truncated = true;
        }
        Ok(())
    }

    fn check_sequence(&self, sequence: u64) -> Result<(), String> {
        if sequence != self.last_sequence + 1 {
            return Err(format!(
                "run event sequence gap: expected {}, observed {sequence}",
                self.last_sequence + 1
            ));
        }
        Ok(())
    }

    fn node(&self, node: &str) -> Result<&NodeView, String> {
        self.nodes
            .get(node)
            .ok_or_else(|| format!("event names node {node} outside its plan"))
    }

    fn node_mut(&mut self, node: &str) -> Result<&mut NodeView, String> {
        self.nodes
            .get_mut(node)
            .ok_or_else(|| format!("event names node {node} outside its plan"))
    }

    fn add_uncertainty(&mut self, message: String) {
        if self.summary.uncertainties.len() < 128 && !self.summary.uncertainties.contains(&message)
        {
            self.summary.uncertainties.push(message);
        }
    }

    fn refresh_summary(&mut self) {
        let mut failures: Vec<_> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                matches!(
                    node.outcome,
                    Some(NodeOutcome::Failed | NodeOutcome::TimedOut)
                )
            })
            .map(|(id, node)| (node.terminal_sequence.unwrap_or(u64::MAX), id.clone()))
            .collect();
        failures.sort();
        self.summary.first_failure = failures.first().map(|(_, id)| id.clone());
        self.summary.independent_failures =
            failures.iter().skip(1).map(|(_, id)| id.clone()).collect();
        self.summary.cancelled = self
            .nodes
            .iter()
            .filter(|(_, node)| node.outcome == Some(NodeOutcome::Cancelled))
            .map(|(id, _)| id.clone())
            .collect();
        self.summary.timed_out = self
            .nodes
            .iter()
            .filter(|(_, node)| node.outcome == Some(NodeOutcome::TimedOut))
            .map(|(id, _)| id.clone())
            .collect();
        self.summary.dependent_skips.clear();
        for (id, node) in &self.nodes {
            if node.outcome != Some(NodeOutcome::Skipped) {
                continue;
            }
            let mut visited = BTreeSet::new();
            let mut pending = node.dependencies.clone();
            let mut causes = BTreeSet::new();
            while let Some(dependency) = pending.pop() {
                if !visited.insert(dependency.clone()) {
                    continue;
                }
                if let Some(upstream) = self.nodes.get(&dependency) {
                    if matches!(
                        upstream.outcome,
                        Some(NodeOutcome::Failed | NodeOutcome::TimedOut | NodeOutcome::Cancelled)
                    ) {
                        causes.insert(dependency.clone());
                    }
                    pending.extend(upstream.dependencies.iter().cloned());
                }
            }
            self.summary
                .dependent_skips
                .insert(id.clone(), causes.into_iter().collect());
        }
        self.summary.outstanding_nodes = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                (node.detached && node.outcome.is_none())
                    || (self.reported_outcome == Some(RunOutcome::Succeeded)
                        && node.outcome != Some(NodeOutcome::Succeeded))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !failures.is_empty() || !self.summary.artifact_failures.is_empty() {
            self.state = RunState::Failed;
        } else if !self.summary.uncertainties.is_empty()
            || !self.summary.outstanding_nodes.is_empty()
        {
            self.state = RunState::Uncertain;
        } else if let Some(outcome) = self.reported_outcome {
            self.state = match outcome {
                RunOutcome::Succeeded => RunState::Succeeded,
                RunOutcome::Failed => RunState::Failed,
                RunOutcome::Cancelled => RunState::Cancelled,
            };
        } else if self
            .nodes
            .values()
            .any(|node| !node.attempts.is_empty() || node.outcome.is_some() || node.phase.is_some())
        {
            self.state = RunState::Running;
        } else {
            self.state = RunState::Pending;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn plan() -> RunPlanMetadata {
        RunPlanMetadata {
            schema_version: RUN_PLAN_V1.into(),
            run_id: "run-test".into(),
            operation_id: "operation-test".into(),
            workspace_root: "/test/workspace".into(),
            workspace_id: "workspace-test".into(),
            input_provenance: BTreeMap::new(),
            roots: vec!["dependent".into()],
            dependencies: BTreeMap::from([
                ("leaf".into(), vec![]),
                ("dependent".into(), vec!["leaf".into()]),
            ]),
            logs_captured: true,
            uncaptured_nodes: Vec::new(),
        }
    }

    #[test]
    fn child_output_cannot_forge_execution_events() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(1, &Event::node_started("leaf")).expect("start");
        view.apply(
            2,
            &Event::StdoutChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8(
                    "{\"type\":\"run_completed\",\"success\":true}",
                ),
            },
        )
        .expect("bytes");
        assert_eq!(view.state, RunState::Running);
        assert!(!view.terminal_receipt_observed);
    }

    #[test]
    fn causal_summary_separates_failure_and_dependent_skip() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(
            1,
            &Event::node_finished("leaf", Some(1), NodeOutcome::Failed, None),
        )
        .expect("failure");
        view.apply(
            2,
            &Event::node_finished("dependent", None, NodeOutcome::Skipped, None),
        )
        .expect("skip");
        view.apply(3, &Event::run_completed(false))
            .expect("terminal");
        assert_eq!(view.summary.first_failure.as_deref(), Some("leaf"));
        assert!(view.summary.independent_failures.is_empty());
        assert_eq!(view.summary.dependent_skips["dependent"], vec!["leaf"]);
    }

    #[test]
    fn missing_events_and_unplanned_nodes_are_errors() {
        let mut view = RunView::new(plan()).expect("view");
        assert!(view.apply(2, &Event::node_started("leaf")).is_err());
        assert!(view.apply(1, &Event::node_started("foreign")).is_err());
        assert_eq!(view.last_sequence, 0);
    }

    #[test]
    fn remote_and_artifact_evidence_cannot_cross_run_identities() {
        let mut view = RunView::new(plan()).expect("view");
        let event = Event::ClientDetached {
            run_id: "another-run".into(),
            node: "leaf".into(),
            attempt_id: "attempt".into(),
            sequence: 1,
        };
        assert!(view.apply(1, &event).is_err());
        assert_eq!(view.last_sequence, 0);
        assert_eq!(view.state, RunState::Pending);
    }

    #[test]
    fn artifact_failure_overrides_successful_processes() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(1, &Event::node_exited("leaf", Some(0)))
            .expect("leaf");
        view.apply(2, &Event::node_exited("dependent", Some(0)))
            .expect("root");
        view.apply(3, &Event::run_completed(true))
            .expect("terminal");
        assert_eq!(view.state, RunState::Succeeded);
        view.artifact_failure(4, "leaf", "digest mismatch")
            .expect("artifact");
        assert_eq!(view.state, RunState::Failed);
    }

    #[test]
    fn deterministic_replay_matches_live_state() {
        let events = [
            Event::node_started("leaf"),
            Event::node_exited("leaf", Some(0)),
            Event::node_started("dependent"),
            Event::node_exited("dependent", Some(0)),
            Event::run_completed(true),
        ];
        let mut live = RunView::new(plan()).expect("live");
        let mut replay = RunView::new(plan()).expect("replay");
        for (index, event) in events.iter().enumerate() {
            live.apply(index as u64 + 1, event).expect("live event");
        }
        let wire = serde_json::to_string(&events).expect("wire");
        let decoded: Vec<Event> = serde_json::from_str(&wire).expect("decoded");
        for (index, event) in decoded.iter().enumerate() {
            replay.apply(index as u64 + 1, event).expect("replay event");
        }
        assert_eq!(live, replay);
    }

    #[test]
    fn independently_observed_remote_terminal_resolves_detachment_uncertainty() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(1, &Event::node_started("leaf")).expect("start");
        view.apply(
            2,
            &Event::ClientDetached {
                run_id: "run-test".into(),
                node: "leaf".into(),
                attempt_id: "attempt-test".into(),
                sequence: 2,
            },
        )
        .expect("detach");
        assert_eq!(view.state, RunState::Uncertain);
        view.apply(3, &Event::node_exited("leaf", Some(0)))
            .expect("terminal receipt");
        view.apply(4, &Event::node_exited("dependent", Some(0)))
            .expect("root");
        view.apply(5, &Event::run_completed(true))
            .expect("run terminal");
        assert_eq!(view.state, RunState::Succeeded);
        assert!(view.summary.uncertainties.is_empty());
    }

    #[test]
    fn late_remote_terminal_resolves_prior_successful_run_uncertainty() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(1, &Event::node_started("leaf")).expect("start");
        view.apply(
            2,
            &Event::ClientDetached {
                run_id: "run-test".into(),
                node: "leaf".into(),
                attempt_id: "attempt-test".into(),
                sequence: 2,
            },
        )
        .expect("detach");
        view.apply(3, &Event::node_exited("dependent", Some(0)))
            .expect("other terminal");
        view.apply(4, &Event::run_completed(true))
            .expect("run terminal");
        assert_eq!(view.state, RunState::Uncertain);
        assert_eq!(view.summary.outstanding_nodes, vec!["leaf"]);
        assert_eq!(view.reported_outcome, Some(RunOutcome::Succeeded));
        view.apply(5, &Event::node_exited("leaf", Some(0)))
            .expect("late authoritative receipt");
        assert_eq!(view.state, RunState::Succeeded);
        assert!(view.summary.outstanding_nodes.is_empty());
        assert!(view.summary.uncertainties.is_empty());
    }

    #[test]
    fn late_remote_failure_remains_failed_and_recorded_uncertainty_is_not_erased() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(1, &Event::node_exited("dependent", Some(0)))
            .expect("other terminal");
        view.apply(2, &Event::run_completed(true))
            .expect("run terminal");
        assert_eq!(view.state, RunState::Uncertain);
        view.uncertainty(3, "recorder lost independent evidence")
            .expect("uncertainty");
        view.apply(4, &Event::node_exited("leaf", Some(1)))
            .expect("late failure");
        assert_eq!(view.state, RunState::Failed);
        assert_eq!(view.summary.first_failure.as_deref(), Some("leaf"));
        assert_eq!(
            view.summary.uncertainties,
            vec!["recorder lost independent evidence"]
        );
    }

    #[test]
    fn late_reconciliation_preserves_authoritative_cancellation() {
        let mut view = RunView::new(plan()).expect("view");
        view.apply(
            1,
            &Event::ClientDetached {
                run_id: "run-test".into(),
                node: "leaf".into(),
                attempt_id: "attempt-test".into(),
                sequence: 1,
            },
        )
        .expect("detach");
        view.apply(
            2,
            &Event::RunCompleted {
                success: false,
                run_id: Some("run-test".into()),
                status: Some(RunOutcome::Cancelled),
                duration_ms: None,
                started_at: None,
                finished_at: None,
            },
        )
        .expect("cancelled run");
        assert_eq!(view.state, RunState::Uncertain);
        view.apply(
            3,
            &Event::node_finished(
                "leaf",
                None,
                NodeOutcome::Cancelled,
                Some("operator cancellation accepted".into()),
            ),
        )
        .expect("late cancellation receipt");
        assert_eq!(view.state, RunState::Cancelled);
        assert_eq!(view.reported_outcome, Some(RunOutcome::Cancelled));
        assert_eq!(view.summary.cancelled, vec!["leaf"]);
    }

    #[test]
    fn artifact_projection_is_bounded_during_replay_without_hiding_failures() {
        use crate::artifact_ref::{ARTIFACT_REF_V1, ArtifactLocation};
        let mut live = RunView::new(plan()).expect("live view");
        let mut replay = RunView::new(plan()).expect("replayed view");
        let count = MAX_NODE_ARTIFACTS + 19;
        let digest = "a".repeat(64);
        for index in 0..count {
            let artifact = ArtifactRef {
                schema_version: ARTIFACT_REF_V1.into(),
                name: format!("recording-{index}"),
                producer: "test".into(),
                kind: "diagnostic-recording".into(),
                media_type: "application/json".into(),
                digest: format!("sha256:{digest}"),
                location: ArtifactLocation::LocalCas {
                    object: format!("objects/{digest}/recording-{index}.json"),
                },
                verified: false,
            };
            live.declare_artifact(index as u64 + 1, "leaf", &artifact)
                .expect("declaration");
            let persisted = serde_json::to_string(&artifact).expect("wire artifact");
            let decoded: ArtifactRef = serde_json::from_str(&persisted).expect("decoded artifact");
            replay
                .declare_artifact(index as u64 + 1, "leaf", &decoded)
                .expect("replayed declaration");
        }
        assert_eq!(live, replay);
        assert_eq!(replay.last_sequence, count as u64);
        assert_eq!(replay.nodes["leaf"].artifacts.len(), MAX_NODE_ARTIFACTS);
        assert!(replay.nodes["leaf"].artifacts_truncated);
        replay
            .artifact_failure(
                count as u64 + 1,
                "leaf",
                "unretained artifact failed verification",
            )
            .expect("failure evidence");
        assert_eq!(replay.state, RunState::Failed);
        assert_eq!(
            replay.summary.artifact_failures["leaf"],
            "unretained artifact failed verification"
        );
    }
}
