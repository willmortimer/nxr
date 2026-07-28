//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid/DOT rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`plan_exec`] — versioned [`ExecutionPlan`] envelope
//! - [`events`] — typed execution event bus (`Event` / [`EventSink`])
//! - [`scheduler`] — ready-queue scheduler with job limit ([`Scheduler`])

pub mod action_key;
pub mod context;
pub mod duration;
pub mod events;
pub mod graph;
pub mod memory;
pub mod plan_exec;
pub mod planner;
pub mod process;
pub mod resolve;
pub mod resources;
pub mod run_events;
pub mod scheduler;
pub mod schema;
pub mod secrets;
pub mod selectors;

pub use action_key::{WorkspaceCachePlan, WorkspaceCachePlanOptions, build_workspace_cache_plan};
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
pub use graph::{GraphError, TaskGraph, render_dot, render_mermaid, render_text};
pub use memory::{MemoryParseError, parse_memory};
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
    AppListingMetadata, ContextEnvironment, ContextEnvironmentMode, ContextSecretRef, EnvInput,
    EnvInputBinding, ExecutionContext, IoIntensity, MAX_SUPPORTED_SCHEMA_VERSION, SCHEMA_VERSION,
    SCHEMA_VERSION_V2, SchemaError, SecretDelivery, SecretProvider, TaskCache, TaskCacheMode,
    TaskCacheSecretPolicy, TaskDefinition, TaskDocument, TaskInputBinding, TaskInputs, TaskOutput,
    TaskOutputMode, TaskResources, WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION,
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
