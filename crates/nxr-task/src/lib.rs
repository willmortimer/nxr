//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`scheduler`] — scaffold for later execution

pub mod graph;
pub mod planner;
pub mod scheduler;
pub mod schema;

pub use graph::{GraphError, TaskGraph, render_mermaid, render_text};
pub use planner::{PlanError, plan_mermaid, plan_serial, plan_text};
pub use schema::{
    SCHEMA_VERSION, SchemaError, TaskDefinition, TaskDocument, validate_schema_version,
};
