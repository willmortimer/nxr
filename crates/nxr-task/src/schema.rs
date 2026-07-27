//! Versioned task schema types and validation helpers.
//!
//! The envelope is `schema_version` plus a map of task name → [`TaskDefinition`].
//! Field names in JSON match the flake metadata vocabulary (`dependsOn`,
//! `workingDirectory`). Schema v1 tolerates unknown task fields via serde
//! defaults; schema v2 rejects unknown document and task fields at parse time.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::process::ProcessDefinition;
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Default major version for newly constructed task documents.
pub const SCHEMA_VERSION: u32 = 1;

/// Schema v2 major version (strict parse; see `docs/TASK_SCHEMA_V2.md`).
pub const SCHEMA_VERSION_V2: u32 = 2;

/// Highest task document major version this crate can parse.
pub const MAX_SUPPORTED_SCHEMA_VERSION: u32 = SCHEMA_VERSION_V2;

/// Run children in the caller's invocation directory.
pub const WORKING_DIRECTORY_INVOCATION: &str = "invocation";

/// Run children at the discovered flake root.
pub const WORKING_DIRECTORY_FLAKE_ROOT: &str = "flake-root";

/// Errors produced while validating a task schema document.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SchemaError {
    /// Document major version is not supported by this crate.
    #[error("unsupported task schema version {found}; expected major version {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    /// `workingDirectory` was empty.
    #[error("task {task}: workingDirectory must not be empty")]
    EmptyWorkingDirectory { task: String },
    /// `workingDirectory` used an absolute path (only relative paths are allowed).
    #[error(
        "task {task}: workingDirectory must be {WORKING_DIRECTORY_INVOCATION}, \
         {WORKING_DIRECTORY_FLAKE_ROOT}, or a relative path (got absolute path {value})"
    )]
    AbsoluteWorkingDirectory { task: String, value: String },
    /// `workingDirectory` traversed parent directories (`..`).
    #[error("task {task}: workingDirectory must not traverse parent directories (got {value})")]
    ParentTraversalWorkingDirectory { task: String, value: String },
    /// A `discoveryInputs` entry is not a valid repository-relative path.
    #[error("discoveryInputs[{index}]: {message}")]
    InvalidDiscoveryInput { index: usize, message: String },
    /// A task `paths` entry is not a valid repository-relative path.
    #[error("task {task}: paths[{index}]: {message}")]
    InvalidTaskPath {
        task: String,
        index: usize,
        message: String,
    },
    /// A task `timeout` or `terminationGracePeriod` string is invalid.
    #[error("task {task}: {message}")]
    InvalidTimeout { task: String, message: String },
    /// JSON did not deserialize into a supported task document shape.
    #[error("task document did not match schema: {message}")]
    InvalidDocument { message: String },
    /// Task references a context name that is not defined in `contexts`.
    #[error("task {task}: unknown context `{context}`")]
    UnknownContext { task: String, context: String },
    /// A task `inputs.paths` entry is not a valid repository-relative path.
    #[error("task {task}: inputs.paths[{index}]: {message}")]
    InvalidInputPath {
        task: String,
        index: usize,
        message: String,
    },
    /// A task `outputs` path is not a valid repository-relative path.
    #[error("task {task}: outputs[{index}].path: {message}")]
    InvalidOutputPath {
        task: String,
        index: usize,
        message: String,
    },
    /// Task resource metadata is invalid.
    #[error("task {task}: {message}")]
    InvalidResources { task: String, message: String },
    /// Context metadata is invalid.
    #[error("context {context}: {message}")]
    InvalidContext { context: String, message: String },
    /// Context secret slot metadata is invalid.
    #[error("context {context}: secrets.{slot}: {message}")]
    InvalidSecret {
        context: String,
        slot: String,
        message: String,
    },
    /// Task or context shell name is invalid.
    #[error("{scope}: {message}")]
    InvalidShell { scope: String, message: String },
}

/// Versioned task document: `schema_version` plus named task definitions.
///
/// Optional [`Self::apps`] is additive listing metadata for flake apps (category
/// and similar). It does not define or replace `apps.<system>.*` outputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskDefinition>,
    /// Optional per-app listing metadata keyed by flake app leaf name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, AppListingMetadata>,
    /// Extra flake-root-relative paths hashed into the discovery cache key.
    #[serde(
        default,
        rename = "discoveryInputs",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub discovery_inputs: Vec<String>,
    /// Named execution contexts (schema v2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, ExecutionContext>,
    /// Long-running process nodes (schema v2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub processes: BTreeMap<String, ProcessDefinition>,
}

/// Listing-only metadata for a flake app leaf (not an operation definition).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppListingMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl TaskDocument {
    /// Supported major schema version for this document type.
    pub const SCHEMA_VERSION: u32 = SCHEMA_VERSION;

    /// Create a document with the current schema version.
    #[must_use]
    pub fn new(tasks: BTreeMap<String, TaskDefinition>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            tasks,
            apps: BTreeMap::new(),
            discovery_inputs: Vec::new(),
            contexts: BTreeMap::new(),
            processes: BTreeMap::new(),
        }
    }

    /// Validate schema version and task field constraints.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when the major version is unsupported or a task
    /// field fails validation (for example an absolute `workingDirectory`).
    pub fn validate(&self) -> Result<(), SchemaError> {
        validate_schema_version(self.schema_version)?;
        for (index, input) in self.discovery_inputs.iter().enumerate() {
            nxr_core::validate_repo_relative_path("discoveryInputs", input).map_err(|error| {
                SchemaError::InvalidDiscoveryInput {
                    index,
                    message: error.to_string(),
                }
            })?;
        }
        if self.schema_version == SCHEMA_VERSION_V2 {
            for (context_name, context) in &self.contexts {
                validate_execution_context(context_name, context)?;
            }
        }
        for (task, definition) in &self.tasks {
            if let Some(context) = &definition.context
                && !self.contexts.contains_key(context)
            {
                return Err(SchemaError::UnknownContext {
                    task: task.clone(),
                    context: context.clone(),
                });
            }
            if self.schema_version == SCHEMA_VERSION_V2 {
                validate_task_v2_semantics(task, definition)?;
            }
            if let Some(working_directory) = &definition.working_directory {
                validate_working_directory(task, working_directory)?;
            }
            for (index, path) in definition.paths.iter().enumerate() {
                nxr_core::validate_repo_relative_path("paths", path).map_err(|error| {
                    SchemaError::InvalidTaskPath {
                        task: task.clone(),
                        index,
                        message: error.to_string(),
                    }
                })?;
            }
            if let Some(timeout) = &definition.timeout {
                crate::duration::parse_duration(timeout).map_err(|error| {
                    SchemaError::InvalidTimeout {
                        task: task.clone(),
                        message: error.to_string(),
                    }
                })?;
            }
            if let Some(grace) = &definition.termination_grace_period {
                crate::duration::parse_duration(grace).map_err(|error| {
                    SchemaError::InvalidTimeout {
                        task: task.clone(),
                        message: error.to_string(),
                    }
                })?;
            }
        }
        Ok(())
    }
}

/// Single task definition (MVP fields).
///
/// `app` is required and names the flake app leaf this task runs. Optional
/// fields mirror the flake-parts / Nix metadata vocabulary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Flake app leaf name (`apps.<system>.<name>`).
    pub app: String,

    #[serde(
        default,
        rename = "workingDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub working_directory: Option<String>,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    /// When true, the node requires exclusive terminal access (stdin inherited;
    /// cannot run concurrently with other nodes or multiplexed output).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interactive: bool,

    /// Optional repository-relative path roots for conservative affected analysis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// Optional wall-clock timeout for this task's process (e.g. `10m`, `30s`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Grace period after timeout/interrupt before SIGKILL (e.g. `5s`).
    #[serde(
        default,
        rename = "terminationGracePeriod",
        skip_serializing_if = "Option::is_none"
    )]
    pub termination_grace_period: Option<String>,

    /// Declared inputs for cache-key fingerprinting (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<TaskInputs>,

    /// Declared workspace outputs for result caching (schema v2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<TaskOutput>,

    /// Opt-in task result cache policy (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<TaskCache>,

    /// Resource reservations and exclusivity locks (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<TaskResources>,

    /// Optional devShell name for a shell-only execution context (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,

    /// Optional named execution context reference (schema v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// Declared inputs for cache-key fingerprinting and structured wiring (schema v2).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskInputs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvInput>,
    #[serde(
        default,
        rename = "includeGitState",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub include_git_state: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, TaskInputBinding>,
}

/// Environment variable name or structured binding for cache fingerprinting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvInput {
    /// Bare environment variable name.
    Name(String),
    /// Structured binding with required/secret metadata.
    Binding(EnvInputBinding),
}

/// Structured environment input binding (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvInputBinding {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// Named binding to an upstream task output (`<task>.<output>`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskInputBinding {
    pub from: String,
}

/// Declared workspace output artifact (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskOutput {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskOutputMode>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// How a cached workspace artifact is restored.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskOutputMode {
    Replace,
    Merge,
    VerifyOnly,
    Report,
}

/// Opt-in task result cache policy (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskCacheMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<bool>,
}

/// Task result cache scope (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCacheMode {
    Disabled,
    Local,
    SharedRead,
    Shared,
}

/// Resource reservations and exclusivity locks (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Informational only; does not affect scheduling today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<IoIntensity>,
    /// Informational only; does not affect scheduling today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusive: Vec<String>,
}

/// Relative I/O intensity for scheduling heuristics (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IoIntensity {
    Light,
    Normal,
    Heavy,
}

/// Named execution context bundling shell, environment, secrets, and confirm (schema v2).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ContextEnvironment>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, ContextSecretRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
}

/// Environment policy for an execution context (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextEnvironment {
    pub mode: ContextEnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unset: Vec<String>,
}

/// Environment inheritance mode for execution contexts (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextEnvironmentMode {
    Inherit,
    Clean,
}

/// Logical secret reference for an execution context (schema v2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextSecretRef {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<SecretDelivery>,
    #[serde(default)]
    pub provider: SecretProvider,
}

/// Runtime secret provider for resolving a logical `ref` (schema v2).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretProvider {
    #[default]
    Env,
    File,
    Sops,
    #[serde(rename = "sops-nix")]
    SopsNix,
}

/// How a secret reference is delivered to a child process (schema v2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretDelivery {
    Env,
    File,
    Stdin,
}

impl TaskDefinition {
    /// Create a minimal task that runs `app` with no dependencies.
    #[must_use]
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            description: None,
            depends_on: Vec::new(),
            app: app.into(),
            working_directory: None,
            hidden: false,
            category: None,
            aliases: Vec::new(),
            interactive: false,
            paths: Vec::new(),
            timeout: None,
            termination_grace_period: None,
            inputs: None,
            outputs: Vec::new(),
            cache: None,
            resources: None,
            shell: None,
            context: None,
        }
    }
}

fn validate_task_v2_semantics(task: &str, definition: &TaskDefinition) -> Result<(), SchemaError> {
    if let Some(shell) = &definition.shell {
        validate_shell_name(&format!("task {task}"), shell)?;
    }
    if let Some(inputs) = &definition.inputs {
        for (index, path) in inputs.paths.iter().enumerate() {
            nxr_core::validate_repo_relative_path("inputs.paths", path).map_err(|error| {
                SchemaError::InvalidInputPath {
                    task: task.to_owned(),
                    index,
                    message: error.to_string(),
                }
            })?;
        }
    }
    for (index, output) in definition.outputs.iter().enumerate() {
        nxr_core::validate_repo_relative_path("outputs.path", &output.path).map_err(|error| {
            SchemaError::InvalidOutputPath {
                task: task.to_owned(),
                index,
                message: error.to_string(),
            }
        })?;
    }
    if let Some(resources) = &definition.resources {
        validate_task_resources(task, resources)?;
    }
    Ok(())
}

fn validate_execution_context(
    context_name: &str,
    context: &ExecutionContext,
) -> Result<(), SchemaError> {
    if let Some(shell) = &context.shell {
        validate_shell_name(&format!("context {context_name}"), shell)?;
    }
    if let Some(environment) = &context.environment {
        validate_context_environment(context_name, environment)?;
    }
    for (slot, secret) in &context.secrets {
        if secret.reference.trim().is_empty() {
            return Err(SchemaError::InvalidSecret {
                context: context_name.to_owned(),
                slot: slot.clone(),
                message: "ref must not be empty".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_context_environment(
    context_name: &str,
    environment: &ContextEnvironment,
) -> Result<(), SchemaError> {
    for name in environment
        .keep
        .iter()
        .chain(environment.unset.iter())
        .chain(environment.set.keys())
    {
        if name.trim().is_empty() {
            return Err(SchemaError::InvalidContext {
                context: context_name.to_owned(),
                message: "environment variable names must not be empty".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_task_resources(task: &str, resources: &TaskResources) -> Result<(), SchemaError> {
    if let Some(cpu) = resources.cpu
        && cpu == 0
    {
        return Err(SchemaError::InvalidResources {
            task: task.to_owned(),
            message: "resources.cpu must be greater than zero when set".to_owned(),
        });
    }
    if let Some(memory) = &resources.memory {
        crate::parse_memory(memory).map_err(|error| SchemaError::InvalidResources {
            task: task.to_owned(),
            message: error.to_string(),
        })?;
    }
    for (index, lock) in resources.exclusive.iter().enumerate() {
        if lock.trim().is_empty() {
            return Err(SchemaError::InvalidResources {
                task: task.to_owned(),
                message: format!("resources.exclusive[{index}] must not be empty"),
            });
        }
    }
    Ok(())
}

fn validate_shell_name(scope: &str, shell: &str) -> Result<(), SchemaError> {
    if shell.trim().is_empty() {
        return Err(SchemaError::InvalidShell {
            scope: scope.to_owned(),
            message: "shell name must not be empty".to_owned(),
        });
    }
    Ok(())
}

/// Validate a task `workingDirectory` token or relative path.
///
/// Accepted values: [`WORKING_DIRECTORY_INVOCATION`], [`WORKING_DIRECTORY_FLAKE_ROOT`],
/// or a non-empty project-relative path. Absolute paths are rejected.
///
/// # Errors
///
/// Returns [`SchemaError::EmptyWorkingDirectory`],
/// [`SchemaError::AbsoluteWorkingDirectory`], or
/// [`SchemaError::ParentTraversalWorkingDirectory`] when the value is invalid.
pub fn validate_working_directory(task: &str, value: &str) -> Result<(), SchemaError> {
    if value.is_empty() {
        return Err(SchemaError::EmptyWorkingDirectory {
            task: task.to_owned(),
        });
    }
    if value == WORKING_DIRECTORY_INVOCATION || value == WORKING_DIRECTORY_FLAKE_ROOT {
        return Ok(());
    }
    if Path::new(value).is_absolute() {
        return Err(SchemaError::AbsoluteWorkingDirectory {
            task: task.to_owned(),
            value: value.to_owned(),
        });
    }
    if Path::new(value)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SchemaError::ParentTraversalWorkingDirectory {
            task: task.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Reject unsupported major schema versions.
///
/// # Errors
///
/// Returns [`SchemaError::UnsupportedVersion`] when `version` is not 1 or 2.
pub fn validate_schema_version(version: u32) -> Result<(), SchemaError> {
    if version == SCHEMA_VERSION || version == SCHEMA_VERSION_V2 {
        Ok(())
    } else {
        Err(SchemaError::UnsupportedVersion {
            found: version,
            expected: SCHEMA_VERSION,
        })
    }
}

/// Parse a versioned task document from JSON (for example `nix eval --json` output).
///
/// Schema v1 tolerates unknown task fields. Schema v2 rejects unknown document
/// and task fields at parse time.
///
/// # Errors
///
/// Returns [`SchemaError::InvalidDocument`] on serde failures,
/// [`SchemaError::UnsupportedVersion`] for unsupported majors, or other
/// [`SchemaError`] variants from [`TaskDocument::validate`].
pub fn parse_task_document(value: &JsonValue) -> Result<TaskDocument, SchemaError> {
    let version = value
        .get("schema_version")
        .and_then(JsonValue::as_u64)
        .map_or(SCHEMA_VERSION, |version| {
            u32::try_from(version).unwrap_or(0)
        });
    validate_schema_version(version)?;

    let doc: TaskDocument = match version {
        SCHEMA_VERSION => serde_json::from_value(value.clone()).map_err(|source| {
            SchemaError::InvalidDocument {
                message: source.to_string(),
            }
        })?,
        SCHEMA_VERSION_V2 => {
            let strict: TaskDocumentV2Strict =
                serde_json::from_value(value.clone()).map_err(|source| {
                    SchemaError::InvalidDocument {
                        message: source.to_string(),
                    }
                })?;
            strict.into()
        }
        _ => {
            return Err(SchemaError::UnsupportedVersion {
                found: version,
                expected: SCHEMA_VERSION,
            });
        }
    };
    doc.validate()?;
    Ok(doc)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDocumentV2Strict {
    schema_version: u32,
    #[serde(default)]
    tasks: BTreeMap<String, TaskDefinitionV2Strict>,
    #[serde(default)]
    apps: BTreeMap<String, AppListingMetadata>,
    #[serde(default, rename = "discoveryInputs")]
    discovery_inputs: Vec<String>,
    #[serde(default)]
    contexts: BTreeMap<String, ExecutionContextV2Strict>,
    #[serde(default)]
    processes: BTreeMap<String, ProcessDefinition>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDefinitionV2Strict {
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "dependsOn")]
    depends_on: Vec<String>,
    app: String,
    #[serde(default, rename = "workingDirectory")]
    working_directory: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    interactive: bool,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default, rename = "terminationGracePeriod")]
    termination_grace_period: Option<String>,
    #[serde(default)]
    inputs: Option<TaskInputsV2Strict>,
    #[serde(default)]
    outputs: Vec<TaskOutputV2Strict>,
    #[serde(default)]
    cache: Option<TaskCacheV2Strict>,
    #[serde(default)]
    resources: Option<TaskResourcesV2Strict>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    context: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskInputsV2Strict {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    env: Vec<EnvInputV2Strict>,
    #[serde(default, rename = "includeGitState")]
    include_git_state: bool,
    #[serde(default)]
    bindings: BTreeMap<String, TaskInputBindingV2Strict>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum EnvInputV2Strict {
    Name(String),
    Binding(EnvInputBindingV2Strict),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvInputBindingV2Strict {
    name: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    secret: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskInputBindingV2Strict {
    from: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskOutputV2Strict {
    path: String,
    #[serde(default)]
    mode: Option<TaskOutputMode>,
    #[serde(default)]
    optional: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCacheV2Strict {
    #[serde(default)]
    mode: Option<TaskCacheMode>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    restore: Option<bool>,
    #[serde(default)]
    save: Option<bool>,
    #[serde(default)]
    failures: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskResourcesV2Strict {
    #[serde(default)]
    cpu: Option<u32>,
    #[serde(default)]
    memory: Option<String>,
    #[serde(default)]
    io: Option<IoIntensity>,
    #[serde(default)]
    network: Option<bool>,
    #[serde(default)]
    exclusive: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionContextV2Strict {
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    environment: Option<ContextEnvironmentV2Strict>,
    #[serde(default)]
    secrets: BTreeMap<String, ContextSecretRefV2Strict>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextEnvironmentV2Strict {
    mode: ContextEnvironmentMode,
    #[serde(default)]
    keep: Vec<String>,
    #[serde(default)]
    set: BTreeMap<String, String>,
    #[serde(default)]
    unset: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextSecretRefV2Strict {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    delivery: Option<SecretDelivery>,
    #[serde(default)]
    provider: SecretProvider,
}

impl From<TaskDocumentV2Strict> for TaskDocument {
    fn from(strict: TaskDocumentV2Strict) -> Self {
        Self {
            schema_version: strict.schema_version,
            tasks: strict
                .tasks
                .into_iter()
                .map(|(name, task)| (name, task.into()))
                .collect(),
            apps: strict.apps,
            discovery_inputs: strict.discovery_inputs,
            contexts: strict
                .contexts
                .into_iter()
                .map(|(name, context)| (name, context.into()))
                .collect(),
            processes: strict.processes,
        }
    }
}

impl From<TaskDefinitionV2Strict> for TaskDefinition {
    fn from(strict: TaskDefinitionV2Strict) -> Self {
        Self {
            description: strict.description,
            depends_on: strict.depends_on,
            app: strict.app,
            working_directory: strict.working_directory,
            hidden: strict.hidden,
            category: strict.category,
            aliases: strict.aliases,
            interactive: strict.interactive,
            paths: strict.paths,
            timeout: strict.timeout,
            termination_grace_period: strict.termination_grace_period,
            inputs: strict.inputs.map(Into::into),
            outputs: strict.outputs.into_iter().map(Into::into).collect(),
            cache: strict.cache.map(Into::into),
            resources: strict.resources.map(Into::into),
            shell: strict.shell,
            context: strict.context,
        }
    }
}

impl From<TaskInputsV2Strict> for TaskInputs {
    fn from(strict: TaskInputsV2Strict) -> Self {
        Self {
            paths: strict.paths,
            env: strict.env.into_iter().map(Into::into).collect(),
            include_git_state: strict.include_git_state,
            bindings: strict
                .bindings
                .into_iter()
                .map(|(name, binding)| (name, binding.into()))
                .collect(),
        }
    }
}

impl From<EnvInputV2Strict> for EnvInput {
    fn from(strict: EnvInputV2Strict) -> Self {
        match strict {
            EnvInputV2Strict::Name(name) => Self::Name(name),
            EnvInputV2Strict::Binding(binding) => Self::Binding(binding.into()),
        }
    }
}

impl From<EnvInputBindingV2Strict> for EnvInputBinding {
    fn from(strict: EnvInputBindingV2Strict) -> Self {
        Self {
            name: strict.name,
            required: strict.required,
            secret: strict.secret,
        }
    }
}

impl From<TaskInputBindingV2Strict> for TaskInputBinding {
    fn from(strict: TaskInputBindingV2Strict) -> Self {
        Self { from: strict.from }
    }
}

impl From<TaskOutputV2Strict> for TaskOutput {
    fn from(strict: TaskOutputV2Strict) -> Self {
        Self {
            path: strict.path,
            mode: strict.mode,
            optional: strict.optional,
        }
    }
}

impl From<TaskCacheV2Strict> for TaskCache {
    fn from(strict: TaskCacheV2Strict) -> Self {
        Self {
            mode: strict.mode,
            version: strict.version,
            restore: strict.restore,
            save: strict.save,
            failures: strict.failures,
        }
    }
}

impl From<TaskResourcesV2Strict> for TaskResources {
    fn from(strict: TaskResourcesV2Strict) -> Self {
        Self {
            cpu: strict.cpu,
            memory: strict.memory,
            io: strict.io,
            network: strict.network,
            exclusive: strict.exclusive,
        }
    }
}

impl From<ExecutionContextV2Strict> for ExecutionContext {
    fn from(strict: ExecutionContextV2Strict) -> Self {
        Self {
            shell: strict.shell,
            environment: strict.environment.map(Into::into),
            secrets: strict
                .secrets
                .into_iter()
                .map(|(name, secret)| (name, secret.into()))
                .collect(),
            confirm: strict.confirm,
        }
    }
}

impl From<ContextEnvironmentV2Strict> for ContextEnvironment {
    fn from(strict: ContextEnvironmentV2Strict) -> Self {
        Self {
            mode: strict.mode,
            keep: strict.keep,
            set: strict.set,
            unset: strict.unset,
        }
    }
}

impl From<ContextSecretRefV2Strict> for ContextSecretRef {
    fn from(strict: ContextSecretRefV2Strict) -> Self {
        Self {
            reference: strict.reference,
            delivery: strict.delivery,
            provider: strict.provider,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn round_trip_full_document() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: Some("Run CI".to_owned()),
                depends_on: vec!["fmt".to_owned(), "test".to_owned()],
                app: "test".to_owned(),
                working_directory: Some("flake-root".to_owned()),
                hidden: false,
                category: Some("validation".to_owned()),
                aliases: vec!["gate".to_owned()],
                interactive: false,
                paths: vec!["crates".to_owned()],
                timeout: None,
                termination_grace_period: None,
                inputs: None,
                outputs: Vec::new(),
                cache: None,
                resources: None,
                shell: None,
                context: None,
            },
        );
        let doc = TaskDocument::new(tasks);

        let encoded = serde_json::to_value(&doc).expect("serialize");
        let decoded: TaskDocument = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, doc);
        decoded.validate().expect("schema version 1 is supported");
    }

    #[test]
    fn aliases_default_to_empty() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "test": {
                    "app": "test"
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        let task = doc.tasks.get("test").expect("task present");
        assert!(task.aliases.is_empty());
    }

    #[test]
    fn round_trip_aliases() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "ci": {
                    "app": "ci",
                    "aliases": ["check", "gate"]
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        assert_eq!(
            doc.tasks["ci"].aliases,
            vec!["check".to_owned(), "gate".to_owned()]
        );
    }

    #[test]
    fn depends_on_defaults_to_empty() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "test": {
                    "app": "test"
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        let task = doc.tasks.get("test").expect("task present");
        assert!(task.depends_on.is_empty());
        assert!(!task.hidden);
        assert_eq!(task.app, "test");
    }

    #[test]
    fn interactive_defaults_to_false() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "test": {
                    "app": "test"
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        assert!(!doc.tasks["test"].interactive);
    }

    #[test]
    fn round_trip_interactive() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "debug": {
                    "app": "debug",
                    "interactive": true
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        assert!(doc.tasks["debug"].interactive);
        let encoded = serde_json::to_value(&doc).expect("serialize");
        assert_eq!(encoded["tasks"]["debug"]["interactive"], Value::Bool(true));
    }

    #[test]
    fn rejects_missing_app() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "ci": {
                    "description": "missing app",
                    "dependsOn": ["test"]
                }
            }
        });
        let err = serde_json::from_value::<TaskDocument>(value).expect_err("app required");
        let message = err.to_string();
        assert!(
            message.contains("app") || message.contains("missing field"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn validate_schema_version_accepts_v1_and_v2() {
        validate_schema_version(1).expect("v1 supported");
        validate_schema_version(2).expect("v2 supported");
        TaskDocument::new(BTreeMap::new())
            .validate()
            .expect("new document is valid");
    }

    #[test]
    fn validate_schema_version_rejects_unsupported_major() {
        let err = validate_schema_version(99).expect_err("v99 unsupported");
        assert_eq!(
            err,
            SchemaError::UnsupportedVersion {
                found: 99,
                expected: 1,
            }
        );

        let doc = TaskDocument {
            schema_version: 99,
            tasks: BTreeMap::new(),
            apps: BTreeMap::new(),
            discovery_inputs: Vec::new(),
            contexts: BTreeMap::new(),
            processes: BTreeMap::new(),
        };
        let err = doc.validate().expect_err("major 99 unsupported");
        assert!(matches!(
            err,
            SchemaError::UnsupportedVersion {
                found: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn v1_tolerates_unknown_task_fields() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "test": {
                    "app": "test",
                    "futureField": true
                }
            }
        });
        let doc = parse_task_document(&value).expect("v1 unknown task field tolerated");
        assert_eq!(doc.tasks["test"].app, "test");
    }

    #[test]
    fn v2_rejects_unknown_task_field() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "test": {
                    "app": "test",
                    "futureField": true
                }
            }
        });
        let err = parse_task_document(&value).expect_err("v2 unknown task field rejected");
        assert!(matches!(err, SchemaError::InvalidDocument { .. }));
        let message = err.to_string();
        assert!(
            message.contains("futureField") || message.contains("unknown field"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn v2_rejects_unknown_document_field() {
        let value = json!({
            "schema_version": 2,
            "futureEnvelope": true,
            "tasks": {}
        });
        let err = parse_task_document(&value).expect_err("v2 unknown document field rejected");
        assert!(matches!(err, SchemaError::InvalidDocument { .. }));
    }

    #[test]
    fn v2_accepts_known_fields() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "test": {
                    "app": "test",
                    "inputs": {
                        "paths": ["Cargo.toml"],
                        "env": ["RUSTFLAGS", { "name": "CI", "secret": true }],
                        "includeGitState": false,
                        "bindings": { "report": { "from": "build.junit" } }
                    },
                    "outputs": [
                        { "path": "target/debug", "mode": "replace", "optional": true }
                    ],
                    "cache": {
                        "mode": "local",
                        "version": "1",
                        "restore": true,
                        "save": true,
                        "failures": false
                    },
                    "resources": {
                        "cpu": 2,
                        "memory": "4GiB",
                        "io": "heavy",
                        "network": false,
                        "exclusive": ["cargo-target"]
                    }
                }
            }
        });
        let doc = parse_task_document(&value).expect("v2 known fields accepted");
        assert_eq!(doc.schema_version, 2);
        let task = &doc.tasks["test"];
        let inputs = task.inputs.as_ref().expect("inputs present");
        assert_eq!(inputs.paths, vec!["Cargo.toml".to_owned()]);
        assert_eq!(task.outputs.len(), 1);
        assert_eq!(task.outputs[0].path, "target/debug");
        assert_eq!(
            task.cache.as_ref().and_then(|cache| cache.mode.as_ref()),
            Some(&TaskCacheMode::Local)
        );
        assert_eq!(task.resources.as_ref().and_then(|r| r.cpu), Some(2));
    }

    #[test]
    fn v2_accepts_contexts_and_task_context_ref() {
        let value = json!({
            "schema_version": 2,
            "contexts": {
                "backend": {
                    "shell": "backend",
                    "environment": {
                        "mode": "inherit",
                        "set": { "RUST_LOG": "debug" }
                    }
                },
                "release": {
                    "shell": "release",
                    "environment": {
                        "mode": "clean",
                        "keep": ["HOME", "SSH_AUTH_SOCK"],
                        "set": { "RELEASE_CHANNEL": "stable" }
                    },
                    "secrets": {
                        "DEPLOY_TOKEN": {
                            "ref": "fixture/prod/deploy-token",
                            "delivery": "env"
                        }
                    },
                    "confirm": true
                }
            },
            "tasks": {
                "deploy": {
                    "app": "deploy",
                    "context": "release"
                },
                "integration": {
                    "app": "test",
                    "shell": "backend"
                }
            }
        });
        let doc = parse_task_document(&value).expect("v2 contexts accepted");
        assert_eq!(doc.contexts.len(), 2);
        assert_eq!(
            doc.contexts["release"].secrets["DEPLOY_TOKEN"].reference,
            "fixture/prod/deploy-token"
        );
        assert_eq!(doc.tasks["deploy"].context.as_deref(), Some("release"));
        assert_eq!(doc.tasks["integration"].shell.as_deref(), Some("backend"));
    }

    #[test]
    fn v2_rejects_unknown_context_field() {
        let value = json!({
            "schema_version": 2,
            "contexts": {
                "backend": {
                    "shell": "backend",
                    "futureField": true
                }
            },
            "tasks": {}
        });
        let err = parse_task_document(&value).expect_err("v2 unknown context field rejected");
        assert!(matches!(err, SchemaError::InvalidDocument { .. }));
    }

    #[test]
    fn validate_rejects_unknown_task_context_reference() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "deploy": {
                    "app": "deploy",
                    "context": "missing"
                }
            }
        });
        let err = parse_task_document(&value).expect_err("unknown context rejected");
        assert!(matches!(
            err,
            SchemaError::UnknownContext {
                task,
                context
            } if task == "deploy" && context == "missing"
        ));
    }

    #[test]
    fn validate_working_directory_accepts_tokens_and_relative_paths() {
        validate_working_directory("fmt", WORKING_DIRECTORY_INVOCATION).expect("invocation");
        validate_working_directory("fmt", WORKING_DIRECTORY_FLAKE_ROOT).expect("flake-root");
        validate_working_directory("fmt", "crates/api").expect("relative");
        validate_working_directory("fmt", "deep/down/here").expect("nested relative");
    }

    #[test]
    fn validate_working_directory_rejects_empty_and_absolute_paths() {
        let empty = validate_working_directory("fmt", "").expect_err("empty");
        assert_eq!(
            empty,
            SchemaError::EmptyWorkingDirectory {
                task: "fmt".to_owned(),
            }
        );

        let absolute = validate_working_directory("fmt", "/tmp/project").expect_err("absolute");
        assert_eq!(
            absolute,
            SchemaError::AbsoluteWorkingDirectory {
                task: "fmt".to_owned(),
                value: "/tmp/project".to_owned(),
            }
        );
    }

    #[test]
    fn validate_working_directory_rejects_parent_traversal() {
        let parent = validate_working_directory("fmt", "../outside").expect_err("parent");
        assert_eq!(
            parent,
            SchemaError::ParentTraversalWorkingDirectory {
                task: "fmt".to_owned(),
                value: "../outside".to_owned(),
            }
        );

        let nested =
            validate_working_directory("fmt", "crates/../../outside").expect_err("nested parent");
        assert_eq!(
            nested,
            SchemaError::ParentTraversalWorkingDirectory {
                task: "fmt".to_owned(),
                value: "crates/../../outside".to_owned(),
            }
        );
    }

    #[test]
    fn validate_document_rejects_parent_traversal_working_directory() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "fmt".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "fmt".to_owned(),
                working_directory: Some("../outside".to_owned()),
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
                timeout: None,
                termination_grace_period: None,
                inputs: None,
                outputs: Vec::new(),
                cache: None,
                resources: None,
                shell: None,
                context: None,
            },
        );
        let doc = TaskDocument::new(tasks);
        let err = doc.validate().expect_err("parent traversal rejected");
        assert!(matches!(
            err,
            SchemaError::ParentTraversalWorkingDirectory { .. }
        ));
    }

    #[test]
    fn validate_document_rejects_absolute_working_directory() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "fmt".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "fmt".to_owned(),
                working_directory: Some("/absolute".to_owned()),
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
                timeout: None,
                termination_grace_period: None,
                inputs: None,
                outputs: Vec::new(),
                cache: None,
                resources: None,
                shell: None,
                context: None,
            },
        );
        let doc = TaskDocument::new(tasks);
        let err = doc.validate().expect_err("absolute path rejected");
        assert!(matches!(err, SchemaError::AbsoluteWorkingDirectory { .. }));
    }

    #[test]
    fn validate_document_rejects_escape_discovery_inputs_and_paths() {
        let mut doc = TaskDocument::new(BTreeMap::new());
        doc.discovery_inputs = vec!["../escape".to_owned()];
        assert!(matches!(
            doc.validate().expect_err("discoveryInputs escape"),
            SchemaError::InvalidDiscoveryInput { .. }
        ));

        let mut tasks = BTreeMap::new();
        tasks.insert(
            "fmt".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "fmt".to_owned(),
                working_directory: None,
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: vec!["/abs".to_owned()],
                timeout: None,
                termination_grace_period: None,
                inputs: None,
                outputs: Vec::new(),
                cache: None,
                resources: None,
                shell: None,
                context: None,
            },
        );
        let doc = TaskDocument::new(tasks);
        assert!(matches!(
            doc.validate().expect_err("paths absolute"),
            SchemaError::InvalidTaskPath { .. }
        ));
    }

    #[test]
    fn serialized_field_names_use_camel_case_vocab() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "build".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: vec!["fmt".to_owned()],
                app: "build".to_owned(),
                working_directory: Some("invocation".to_owned()),
                hidden: true,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
                timeout: None,
                termination_grace_period: None,
                inputs: None,
                outputs: Vec::new(),
                cache: None,
                resources: None,
                shell: None,
                context: None,
            },
        );
        let value = serde_json::to_value(TaskDocument::new(tasks)).expect("serialize");
        let task = &value["tasks"]["build"];
        assert!(task.get("dependsOn").is_some());
        assert!(task.get("workingDirectory").is_some());
        assert!(task.get("depends_on").is_none());
        assert_eq!(task["hidden"], Value::Bool(true));
    }

    #[test]
    fn v2_rejects_invalid_resources_cpu() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "build": {
                    "app": "build",
                    "resources": { "cpu": 0 }
                }
            }
        });
        let err = parse_task_document(&value).expect_err("cpu zero rejected");
        assert!(matches!(err, SchemaError::InvalidResources { .. }));
    }

    #[test]
    fn v2_rejects_invalid_memory_and_output_escape() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "build": {
                    "app": "build",
                    "outputs": [{ "path": "../outside" }],
                    "resources": { "memory": "not-memory" }
                }
            }
        });
        let err = parse_task_document(&value).expect_err("invalid v2 semantics");
        assert!(
            matches!(
                err,
                SchemaError::InvalidOutputPath { .. } | SchemaError::InvalidResources { .. }
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn v2_rejects_empty_exclusive_lock_and_secret_ref() {
        let value = json!({
            "schema_version": 2,
            "contexts": {
                "release": {
                    "secrets": {
                        "TOKEN": { "ref": "" }
                    }
                }
            },
            "tasks": {
                "build": {
                    "app": "build",
                    "resources": { "exclusive": [""] }
                }
            }
        });
        let err = parse_task_document(&value).expect_err("empty metadata rejected");
        assert!(
            matches!(
                err,
                SchemaError::InvalidSecret { .. } | SchemaError::InvalidResources { .. }
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn v2_rejects_empty_shell_name() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "build": {
                    "app": "build",
                    "shell": "  "
                }
            }
        });
        let err = parse_task_document(&value).expect_err("empty shell rejected");
        assert!(matches!(err, SchemaError::InvalidShell { .. }));
    }

    #[test]
    fn v2_rejects_input_path_escape() {
        let value = json!({
            "schema_version": 2,
            "tasks": {
                "build": {
                    "app": "build",
                    "inputs": { "paths": ["../escape"] }
                }
            }
        });
        let err = parse_task_document(&value).expect_err("input escape rejected");
        assert!(matches!(err, SchemaError::InvalidInputPath { .. }));
    }
}
