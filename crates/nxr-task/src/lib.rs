//! Task schema, graph planning, and scheduling for nxr.
//!
//! The [`schema`] module defines the versioned V1 task document contract used by
//! later planner and CLI work. Graph, planner, and scheduler modules remain
//! scaffolds until those layers land.

pub mod graph;
pub mod planner;
pub mod scheduler;
pub mod schema;

pub use schema::{
    SCHEMA_VERSION, SchemaError, TaskDefinition, TaskDocument, validate_schema_version,
};
