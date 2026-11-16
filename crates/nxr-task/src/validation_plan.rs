//! Read-only batch plan for one integrated change set.
//!
//! The plan names coverage. It does not start work, and a caller cannot
//! set `authorized` through this constructor.

use serde::Serialize;

use crate::catalog::{EffectClass, OperationRecord};

pub const VALIDATION_PLAN_SCHEMA: &str = "nxr-validation-plan-v1";

/// Inputs the planner already knows. Discovery failure is a coverage fact.
pub struct ValidationPlanInput<'a> {
    pub discovery_failed: bool,
    pub dirty: bool,
    pub changed_paths: &'a [String],
    pub unknown_nodes: &'a [String],
    pub operations: &'a [OperationRecord],
}

/// One operation the batch would consider. Effect is descriptive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlannedOperation {
    pub operation_id: String,
    pub effect: EffectClass,
    pub inputs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanCoverage {
    pub status: String,
    pub affected_paths: Vec<String>,
    pub unresolved_inputs: Vec<String>,
}

/// Server-derived gate. Planning always leaves `authorized` false.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanExecution {
    pub authorized: bool,
    pub requires_authorization: bool,
    pub deploy_allowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidationPlan {
    pub schema: String,
    pub intent: String,
    pub coverage: PlanCoverage,
    pub operations: Vec<PlannedOperation>,
    pub execution: PlanExecution,
}

/// Build a plan whose execution gate is not a grant of authority.
#[must_use]
pub fn validation_plan(input: &ValidationPlanInput<'_>) -> ValidationPlan {
    let empty_inputs: Vec<String> = input
        .operations
        .iter()
        .filter(|operation| operation.inputs.is_empty())
        .map(|operation| operation.operation_id.clone())
        .collect();
    let mut unresolved = Vec::new();
    if input.discovery_failed {
        unresolved.push("discovery".to_owned());
    }
    if input.dirty {
        unresolved.extend(empty_inputs.iter().cloned());
    }
    let status = if input.discovery_failed || (input.dirty && !empty_inputs.is_empty()) {
        "unknown"
    } else if !input.unknown_nodes.is_empty() {
        "partial"
    } else {
        "complete"
    };
    ValidationPlan {
        schema: VALIDATION_PLAN_SCHEMA.to_owned(),
        intent: "integration".to_owned(),
        coverage: PlanCoverage {
            status: status.to_owned(),
            affected_paths: input.changed_paths.to_vec(),
            unresolved_inputs: unresolved,
        },
        operations: input
            .operations
            .iter()
            .map(|operation| PlannedOperation {
                operation_id: operation.operation_id.clone(),
                effect: operation.inferred_effect,
                inputs: operation.inputs.clone(),
            })
            .collect(),
        execution: PlanExecution {
            authorized: false,
            requires_authorization: true,
            deploy_allowed: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Declaration, OperationKind, WorkspaceInstance, operation_record};

    fn operation(name: &str, inputs: Vec<String>) -> OperationRecord {
        let workspace = WorkspaceInstance {
            workspace_id: "nxr".to_owned(),
            workspace_root: "/tmp/nxr".to_owned(),
            source_revision: Some("abc".to_owned()),
        };
        let mut declaration = Declaration::named(OperationKind::Task, name, "flake.nix");
        declaration.inputs = inputs;
        operation_record(&workspace, &declaration)
    }

    #[test]
    fn a_mapped_clean_plan_is_complete_and_unauthorized() {
        let operations = [operation("test", vec!["crates/nxr-task".into()])];
        let paths = ["crates/nxr-task/src/lib.rs".to_owned()];
        let plan = validation_plan(&ValidationPlanInput {
            discovery_failed: false,
            dirty: false,
            changed_paths: &paths,
            unknown_nodes: &[],
            operations: &operations,
        });
        assert_eq!(plan.schema, VALIDATION_PLAN_SCHEMA);
        assert_eq!(plan.coverage.status, "complete");
        assert!(plan.coverage.unresolved_inputs.is_empty());
        assert!(!plan.execution.authorized);
        assert!(!plan.execution.deploy_allowed);
        assert!(plan.execution.requires_authorization);
        assert_eq!(plan.operations[0].effect, EffectClass::LocalBuild);
    }

    #[test]
    fn unknown_nodes_are_partial_and_empty_dirty_inputs_are_unknown() {
        let operations = [operation("fmt-check", Vec::new())];
        let unknown = ["shared".to_owned()];
        let partial = validation_plan(&ValidationPlanInput {
            discovery_failed: false,
            dirty: false,
            changed_paths: &[],
            unknown_nodes: &unknown,
            operations: &operations,
        });
        assert_eq!(partial.coverage.status, "partial");
        let dirty = validation_plan(&ValidationPlanInput {
            discovery_failed: false,
            dirty: true,
            changed_paths: &[],
            unknown_nodes: &unknown,
            operations: &operations,
        });
        assert_eq!(dirty.coverage.status, "unknown");
        assert!(
            dirty
                .coverage
                .unresolved_inputs
                .contains(&operations[0].operation_id)
        );
        let failed = validation_plan(&ValidationPlanInput {
            discovery_failed: true,
            dirty: false,
            changed_paths: &["flake.nix".to_owned()],
            unknown_nodes: &[],
            operations: &[],
        });
        assert_eq!(failed.coverage.status, "unknown");
        assert_eq!(failed.coverage.unresolved_inputs, ["discovery"]);
        assert!(!failed.execution.authorized);
    }
}
