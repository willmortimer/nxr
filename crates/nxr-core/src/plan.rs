//! Versioned plan envelope for `nxr plan --json` and `--dry-run`.

use serde::{Deserialize, Serialize};

/// Plan target kind (V1: flake apps only).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    App,
}

/// How the child process inherits environment variables.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPolicy {
    Inherit,
}

/// Executable invocation recorded in a plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanCommand {
    pub program: String,
    pub arguments: Vec<String>,
}

/// Versioned JSON envelope for `nxr plan --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub schema_version: u32,
    pub kind: PlanKind,
    pub flake: String,
    pub system: String,
    pub target: String,
    pub attr_path: String,
    pub invocation_directory: String,
    pub execution_directory: String,
    pub environment_policy: EnvironmentPolicy,
    pub command: PlanCommand,
    pub forwarded_arguments: Vec<String>,
}

impl Plan {
    pub const SCHEMA_VERSION: u32 = 1;
}
