//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid/DOT rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`plan_exec`] — versioned [`ExecutionPlan`] envelope
//! - [`workflows`] — optional `.nxr/workflows.toml` operation graph
//! - [`execute`] — operation dispatch (native argv, nix availability, winbuild)
//! - [`events`] — typed execution event bus (`Event` / [`EventSink`])
//! - [`scheduler`] — ready-queue scheduler with job limit ([`Scheduler`])

pub mod action_key;
pub mod ci_metadata;
pub mod context;
pub mod duration;
pub mod events;
pub mod execute;
pub mod execution_spec;
pub mod graph;
pub mod identity;
pub mod matrix;
pub mod memory;
pub mod parameters;
pub mod plan_exec;
pub mod planner;
pub mod process;
pub mod resolve;
pub mod resources;
pub mod run_events;
pub mod run_journal;
pub mod scheduler;
pub mod schema;
pub mod secrets;
pub mod selectors;
pub mod workflows;

pub use action_key::{WorkspaceCachePlan, WorkspaceCachePlanOptions, build_workspace_cache_plan};
pub use ci_metadata::{CiRunMetadata, attach_ci_run};
pub use context::{
    AppliedTaskContext, ContextError, NXR_ASSUME_YES_ENV, PlanSecretEntry,
    PlanSecretValuePlaceholder, apply_task_context, enforce_context_confirm,
    merge_spawn_env_overrides, resolve_env_provider_secrets, resolve_env_provider_secrets_with,
    resolve_task_context, secret_delivery_mode, secret_provider_mode,
    serialized_plan_excludes_value,
};
pub use duration::{format_duration, parse_duration};
pub use events::{
    ChunkEncoding, Event, EventSink, NodeOutcome, NullSink, OutputPayload, RecordingSink,
    RunOutcome, event_kind,
};
pub use execute::{
    ExecContext, ExecuteError, ExecutorName, OperationResult, OperationRun, UNAVAILABLE_EXECUTOR,
    execute_operation, run_operation_plan,
};
pub use execution_spec::{
    AdmissionPolicy, CiIdentity, ExecutionSpecV2, OutputDecl, RunEventV2, SpecRun,
    execute_spec_locally, execution_spec_for_task, execution_specs_for_document,
    source_set_from_repo, workflow_definition_digest,
};
pub use graph::{GraphError, TaskGraph, render_dot, render_mermaid, render_text};
pub use identity::{SourceSetV1, operation_id, source_set_digest};
pub use matrix::{
    MATRIX_ENV_PREFIX, MatrixExpansion, MatrixInstance, expand_matrix_roots, expand_matrix_tasks,
    matrix_attr_names, matrix_env_name, resolve_matrix_env, resolve_matrix_values,
    validate_matrix_attrs, validate_task_matrix,
};
pub use memory::{MemoryParseError, parse_memory};
pub use parameters::{
    PARAM_ENV_PREFIX, ParameterError, parameter_env_name, parameter_names,
    parameter_values_from_env, resolve_task_parameter_env, resolve_task_parameter_env_layered,
    resolve_task_parameter_env_with, resolve_task_parameter_values, validate_task_parameters,
};
pub use plan_exec::{
    ArgumentForwarding, EXECUTION_PLAN_SCHEMA_VERSION, ExecutionPlan, FailurePolicy, PlanNode,
    build_execution_plan, build_execution_plan_roots, build_serial_plan,
};
pub use planner::{PlanError, plan_mermaid, plan_serial, plan_serial_union, plan_text};
pub use process::{
    ProcessDefinition, ProcessNameError, ProcessReadiness, ProcessRestart, ReadinessHttp,
    ReadinessTcp, dependency_base_name, parse_processes, sanitize_process_log_name,
    validate_node_id,
};
pub use resolve::{
    ResolveTaskError, enrich_apps_with_listing_metadata, listable_tasks, listable_tasks_filtered,
    resolve_task, resolve_task_name,
};
pub use resources::{NodeResources, ResourceLimits};
pub use run_events::{RunEventDecorator, format_rfc3339_utc};
pub use scheduler::{NodeState, ScheduleOutcome, Scheduler, SchedulerError};
pub use schema::{
    AppFastPathMetadata, AppListingMetadata, ContextEnvironment, ContextEnvironmentMode,
    ContextSecretRef, EnvInput, EnvInputBinding, ExecutionContext, IoIntensity,
    MAX_SUPPORTED_SCHEMA_VERSION, SCHEMA_VERSION, SCHEMA_VERSION_V2, SchemaError, SecretDelivery,
    SecretProvider, TaskCache, TaskCacheMode, TaskCacheSecretPolicy, TaskDefinition, TaskDocument,
    TaskInputBinding, TaskInputs, TaskMatrix, TaskOutput, TaskOutputMode, TaskParameter,
    TaskParameterType, TaskResources, WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION,
    parse_task_document, validate_schema_version, validate_working_directory,
};
pub use secrets::{
    ResolvedSecrets, SecureTempFile, authorize_secret_refs, resolve_context_secrets,
    secret_refs_for_entries,
};
pub use selectors::{
    APP_PREFIX, CATEGORY_PREFIX, CHANGED_SELECTOR, ListSelectorResolution, ParsedSelector,
    SelectorError, TASK_PREFIX, TaskTargetResolution, parse_selector, resolve_list_selector,
    resolve_task_targets,
};
pub use workflows::{
    WORKFLOWS_RELATIVE_PATH, WORKFLOWS_SCHEMA_VERSION, WorkflowDocument, WorkflowError,
    WorkflowPlanMode, WorkflowPlatform, WorkflowTask, load_workflow_document,
    plan_workflow_document, plan_workflows, workflow_mode_from_selector, workflow_path,
};
