//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`plan_exec`] — versioned [`ExecutionPlan`] envelope
//! - [`events`] — typed execution event bus (`Event` / [`EventSink`])
//! - [`scheduler`] — scaffold for later execution

pub mod events;
pub mod graph;
pub mod plan_exec;
pub mod planner;
pub mod scheduler;
pub mod schema;

pub use events::{Event, EventSink, NullSink, RecordingSink, event_kind};
pub use graph::{GraphError, TaskGraph, render_mermaid, render_text};
pub use plan_exec::{
    EXECUTION_PLAN_SCHEMA_VERSION, ExecutionPlan, FailurePolicy, PlanNode, build_execution_plan,
    build_serial_plan,
};
pub use planner::{PlanError, plan_mermaid, plan_serial, plan_text};
pub use schema::{
    SCHEMA_VERSION, SchemaError, TaskDefinition, TaskDocument, validate_schema_version,
};
