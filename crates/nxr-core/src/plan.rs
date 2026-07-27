//! Versioned plan envelope for `nxr plan --json` and `--dry-run`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::env_policy::EnvironmentPolicy;

/// Plan target kind (V1: flake apps only).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    App,
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
    /// Selected `devShell` name when `--shell` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Active dev shell from `NXR_DEV_SHELL` when set in the caller environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_shell: Option<String>,
    pub environment_policy: EnvironmentPolicy,
    /// Execution context selected for this node (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Logical secret refs required by [`Self::context`] (values never serialized).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<PlanSecretRef>,
    /// Non-secret `environment.set` from inherit-mode contexts (applied at spawn).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_env_set: BTreeMap<String, String>,
    pub command: PlanCommand,
    pub forwarded_arguments: Vec<String>,
}

/// Secret metadata in app/task plans (never includes resolved values).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanSecretRef {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub delivery: String,
    pub value: String,
}

impl Plan {
    pub const SCHEMA_VERSION: u32 = 1;
}
