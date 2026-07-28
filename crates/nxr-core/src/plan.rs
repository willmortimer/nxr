//! Versioned plan envelope for `nxr plan --json` and `--dry-run`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::env_policy::EnvironmentPolicy;

/// Plan target kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    App,
    /// Local checkout script (`nxr script` or file-backed live fast path).
    WorkspaceScript,
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
    /// Absolute workspace script path when [`Self::kind`] is [`PlanKind::WorkspaceScript`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_script: Option<String>,
    /// True when the operation runs mutable checkout content (not a store app).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mutable_source: bool,
    /// Store app leaf used when a live workspace fast path is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_app: Option<String>,
    /// How the dev shell environment is applied: `process`, `shell`, or omitted when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_mode: Option<String>,
}

/// Secret metadata in app/task plans (never includes resolved values).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanSecretRef {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub delivery: String,
    #[serde(default = "default_secret_provider")]
    pub provider: String,
    pub value: String,
}

fn default_secret_provider() -> String {
    "env".to_owned()
}

impl Plan {
    pub const SCHEMA_VERSION: u32 = 1;
}
