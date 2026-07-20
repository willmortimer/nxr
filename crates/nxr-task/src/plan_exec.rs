//! Versioned execution-plan envelope built from task definitions.
//!
//! Serial planning reuses [`crate::planner::plan_serial`] for ordering. Parallel
//! wave scheduling is deferred; the serial builder emits one node per wave so
//! later schedulers have a stable envelope shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::events::{Event, EventSink};
use crate::graph::TaskGraph;
use crate::planner::{PlanError, plan_serial};
use crate::schema::TaskDefinition;

/// Supported major version for the execution-plan envelope.
pub const EXECUTION_PLAN_SCHEMA_VERSION: u32 = 1;

/// How the scheduler should react when a node fails.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Stop scheduling new nodes after the first failure.
    #[default]
    FailFast,
    /// Continue running nodes whose dependencies still succeed.
    KeepGoing,
}

impl FailurePolicy {
    /// Exhaustive label for diagnostics / tests.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FailurePolicy::FailFast => "fail_fast",
            FailurePolicy::KeepGoing => "keep_going",
        }
    }
}

/// How trailing CLI args are forwarded onto node apps (V2 freeze).
///
/// Unknown JSON values are rejected by serde; older plans without the field
/// deserialize to [`Self::Root`] via `#[serde(default)]`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentForwarding {
    /// Trailing args go only to the root task's app; dependency nodes get `[]`.
    #[default]
    Root,
}

impl ArgumentForwarding {
    /// Exhaustive label for diagnostics / tests.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ArgumentForwarding::Root => "root",
        }
    }
}

/// One node in an [`ExecutionPlan`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanNode {
    /// Task id (key in the task map).
    pub id: String,
    /// Direct `dependsOn` targets that must complete before this node.
    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// When true, the scheduler runs this node exclusively (no concurrent peers).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interactive: bool,
}

/// Versioned, immutable execution plan for a chosen root.
///
/// `serial_order` is always populated (deterministic Kahn order). `waves` holds
/// optional parallel batches; the serial builder emits one node per wave.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub schema_version: u32,
    /// Root task id requested by the caller.
    pub root: String,
    /// Failure handling policy for a future scheduler.
    pub failure_policy: FailurePolicy,
    /// Trailing CLI argument forwarding policy (V2 freeze: root only).
    #[serde(default)]
    pub argument_forwarding: ArgumentForwarding,
    /// Nodes in the reachable subgraph (lexicographic id order).
    pub nodes: Vec<PlanNode>,
    /// Deterministic serial execution order (dependencies before dependents).
    pub serial_order: Vec<String>,
    /// Parallel-ready waves; serial plans use one task id per wave.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waves: Vec<Vec<String>>,
}

impl ExecutionPlan {
    /// Supported major schema version for this envelope.
    pub const SCHEMA_VERSION: u32 = EXECUTION_PLAN_SCHEMA_VERSION;

    /// Task ids present in the plan (same as [`Self::nodes`] ids).
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|n| n.id.as_str())
    }

    /// Task ids marked `interactive` in the reachable subgraph.
    pub fn interactive_node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes
            .iter()
            .filter(|n| n.interactive)
            .map(|n| n.id.as_str())
    }

    /// Whether any node in the plan requires exclusive interactive execution.
    #[must_use]
    pub fn has_interactive_nodes(&self) -> bool {
        self.nodes.iter().any(|n| n.interactive)
    }
}

/// Build a serial [`ExecutionPlan`] for `root` from task definitions.
///
/// Ordering matches [`plan_serial`]. Each serial step becomes its own wave so
/// the envelope is ready for a later parallel scheduler without changing
/// serial semantics.
///
/// When `sink` is provided, emits [`Event::PlanCreated`] after a successful build.
///
/// # Errors
///
/// Returns the same errors as [`plan_serial`].
pub fn build_execution_plan(
    tasks: &BTreeMap<String, TaskDefinition>,
    root: &str,
    failure_policy: FailurePolicy,
    sink: Option<&mut dyn EventSink>,
) -> Result<ExecutionPlan, PlanError> {
    let serial_order = plan_serial(tasks, root)?;
    let graph = TaskGraph::subgraph(tasks, root)?;

    let nodes: Vec<PlanNode> = graph
        .node_ids()
        .filter_map(|id| {
            let definition = tasks.get(id)?;
            Some(PlanNode {
                id: id.to_owned(),
                depends_on: graph
                    .dependencies(id)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default(),
                interactive: definition.interactive,
            })
        })
        .collect();

    // Serial case: one node per wave preserves order and envelope shape for P1.
    let waves: Vec<Vec<String>> = serial_order.iter().cloned().map(|id| vec![id]).collect();

    let plan = ExecutionPlan {
        schema_version: ExecutionPlan::SCHEMA_VERSION,
        root: root.to_owned(),
        failure_policy,
        argument_forwarding: ArgumentForwarding::Root,
        nodes,
        serial_order,
        waves,
    };

    if let Some(sink) = sink {
        sink.emit(Event::PlanCreated {
            root: plan.root.clone(),
            node_count: plan.nodes.len(),
        });
    }

    Ok(plan)
}

/// Convenience: build with [`FailurePolicy::FailFast`] and no event sink.
///
/// # Errors
///
/// Returns the same errors as [`build_execution_plan`].
pub fn build_serial_plan(
    tasks: &BTreeMap<String, TaskDefinition>,
    root: &str,
) -> Result<ExecutionPlan, PlanError> {
    build_execution_plan(tasks, root, FailurePolicy::FailFast, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecordingSink, event_kind};
    use crate::schema::TaskDefinition;

    fn task(deps: &[&str]) -> TaskDefinition {
        let mut def = TaskDefinition::new("app");
        def.depends_on = deps.iter().map(|s| (*s).to_owned()).collect();
        def
    }

    fn diamond() -> BTreeMap<String, TaskDefinition> {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        tasks.insert("d".to_owned(), task(&["b", "c"]));
        tasks
    }

    #[test]
    fn serial_plan_matches_plan_serial_diamond() {
        let tasks = diamond();
        let order = plan_serial(&tasks, "d").expect("plan_serial");
        let plan = build_serial_plan(&tasks, "d").expect("execution plan");

        assert_eq!(plan.schema_version, EXECUTION_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.root, "d");
        assert_eq!(plan.failure_policy, FailurePolicy::FailFast);
        assert_eq!(plan.serial_order, order);
        assert_eq!(plan.serial_order, vec!["a", "b", "c", "d"]);
        assert_eq!(
            plan.waves,
            vec![
                vec!["a".to_owned()],
                vec!["b".to_owned()],
                vec!["c".to_owned()],
                vec!["d".to_owned()],
            ]
        );

        let ids: Vec<_> = plan.node_ids().collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);

        let d = plan.nodes.iter().find(|n| n.id == "d").expect("d");
        assert_eq!(d.depends_on, vec!["b".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn serial_chain_plan() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["b"]));

        let plan = build_serial_plan(&tasks, "c").expect("plan");
        assert_eq!(plan.serial_order, vec!["a", "b", "c"]);
        assert_eq!(plan.waves.len(), 3);
        assert_eq!(FailurePolicy::FailFast.as_str(), "fail_fast");
        assert_eq!(FailurePolicy::KeepGoing.as_str(), "keep_going");
    }

    #[test]
    fn build_emits_plan_created() {
        let tasks = diamond();
        let mut sink = RecordingSink::new();
        let plan = build_execution_plan(&tasks, "d", FailurePolicy::KeepGoing, Some(&mut sink))
            .expect("plan");

        assert_eq!(plan.failure_policy, FailurePolicy::KeepGoing);
        assert_eq!(
            sink.events(),
            &[Event::PlanCreated {
                root: "d".to_owned(),
                node_count: 4,
            }]
        );
        assert_eq!(event_kind(&sink.events()[0]), "plan_created");
    }

    #[test]
    fn execution_plan_json_round_trip() {
        let tasks = diamond();
        let plan = build_execution_plan(&tasks, "d", FailurePolicy::KeepGoing, None).expect("plan");

        let encoded = serde_json::to_value(&plan).expect("serialize");
        assert_eq!(encoded["schema_version"], 1);
        assert_eq!(encoded["failure_policy"], "keep_going");
        assert_eq!(encoded["argument_forwarding"], "root");
        assert!(encoded["nodes"][3].get("dependsOn").is_some());

        let decoded: ExecutionPlan = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, plan);
    }

    #[test]
    fn argument_forwarding_defaults_to_root_when_absent() {
        let json = serde_json::json!({
            "schema_version": 1,
            "root": "d",
            "failure_policy": "fail_fast",
            "nodes": [],
            "serial_order": []
        });
        let plan: ExecutionPlan = serde_json::from_value(json).expect("deserialize");
        assert_eq!(plan.argument_forwarding, ArgumentForwarding::Root);
        assert_eq!(plan.argument_forwarding.as_str(), "root");
    }

    #[test]
    fn interactive_flag_propagates_to_plan_nodes() {
        let mut tasks = diamond();
        tasks.get_mut("b").expect("b").interactive = true;
        let plan = build_serial_plan(&tasks, "d").expect("plan");
        let b = plan.nodes.iter().find(|n| n.id == "b").expect("b");
        assert!(b.interactive);
        assert!(
            !plan
                .nodes
                .iter()
                .find(|n| n.id == "c")
                .expect("c")
                .interactive
        );
        assert_eq!(plan.interactive_node_ids().collect::<Vec<_>>(), vec!["b"]);
        assert!(plan.has_interactive_nodes());
    }

    #[test]
    fn build_propagates_plan_errors() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["a"]));
        let err = build_serial_plan(&tasks, "a").expect_err("cycle");
        assert!(matches!(err, PlanError::Cycle { .. }));
    }

    #[test]
    fn ignores_unrelated_tasks() {
        let mut tasks = diamond();
        tasks.insert("unrelated".to_owned(), task(&[]));
        let plan = build_serial_plan(&tasks, "d").expect("plan");
        assert!(!plan.node_ids().any(|id| id == "unrelated"));
    }
}
