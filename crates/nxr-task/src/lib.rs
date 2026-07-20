//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid/DOT rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`plan_exec`] — versioned [`ExecutionPlan`] envelope
//! - [`events`] — typed execution event bus (`Event` / [`EventSink`])
//! - [`scheduler`] — ready-queue scheduler with job limit ([`Scheduler`])

pub mod events;
pub mod graph;
pub mod plan_exec;
pub mod planner;
pub mod resolve;
pub mod scheduler;
pub mod schema;

pub use events::{
    ChunkEncoding, Event, EventSink, NullSink, OutputPayload, RecordingSink, event_kind,
};
pub use graph::{GraphError, TaskGraph, render_dot, render_mermaid, render_text};
pub use plan_exec::{
    ArgumentForwarding, EXECUTION_PLAN_SCHEMA_VERSION, ExecutionPlan, FailurePolicy, PlanNode,
    build_execution_plan, build_execution_plan_roots, build_serial_plan,
};
pub use planner::{PlanError, plan_mermaid, plan_serial, plan_serial_union, plan_text};
pub use resolve::{
    ResolveTaskError, enrich_apps_with_listing_metadata, listable_tasks, listable_tasks_filtered,
    resolve_task, resolve_task_name,
};
pub use scheduler::{NodeState, ScheduleOutcome, Scheduler, SchedulerError};
pub use schema::{
    AppListingMetadata, SCHEMA_VERSION, SchemaError, TaskDefinition, TaskDocument,
    WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION, validate_schema_version,
    validate_working_directory,
};
