//! Optional `.nxr/workflows.toml` operation graph.
//!
//! The file is not required. Repositories that lack it are unchanged, and bare
//! `nxr <name>` resolution does not consult it. A workflow graph is planned
//! only when the file is present ([`WorkflowPlanMode::IfPresent`]) or an
//! explicit `workflow` / `workflow:<task>` selector asks for it.
//!
//! The file names tasks, dependencies, a platform, and one executor reference.
//! It does not embed build scripts.
//!
//! ```toml
//! schema_version = 1
//!
//! [tasks.prep]
//! platform = "unix" # any | unix | macos | linux | windows; default any
//! depends_on = [] # alias: dependsOn
//! executor = { native = ["nxr-fixture-echo", "prep"] }
//!
//! [tasks.fmt]
//! depends_on = ["prep"]
//! executor = { flake = "fmt" }
//!
//! [tasks.win]
//! platform = "windows"
//! executor = { winbuild = "link-release" }
//! ```
//!
//! `executor` sets exactly one of `flake` (app name), `native` (argv), or
//! `winbuild` (task id).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nxr_core::Operation;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::events::{Event, EventSink};
use crate::graph::{GraphError, TaskGraph};
use crate::identity::SourceSetV1;
use crate::plan_exec::{ArgumentForwarding, ExecutionPlan, FailurePolicy, PlanNode};
use crate::planner::plan_serial_union;
use crate::remote_execution::{ExecutionTransport, submit_execution};
use crate::resources::NodeResources;
use crate::schema::{TaskDefinition, TaskResources};

/// Supported major version for `.nxr/workflows.toml`.
pub const WORKFLOWS_SCHEMA_VERSION: u32 = 1;

/// Repo-relative path of the optional workflow file.
pub const WORKFLOWS_RELATIVE_PATH: &str = ".nxr/workflows.toml";

/// How a caller asked to plan a workflow graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowPlanMode {
    /// Plan when the file exists. A missing file is not an error.
    IfPresent,
    /// `workflow` (entire graph) or `workflow:<task>`.
    ///
    /// A missing file is an error. This is never inferred from a bare name.
    Explicit { root: Option<String> },
}

/// Platform named by a workflow task.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkflowPlatform {
    /// No platform constraint.
    #[default]
    Any,
    /// Unix-like host (`macos` and `linux` both match).
    Unix,
    /// macOS host.
    Macos,
    /// Linux host.
    Linux,
    /// Windows host.
    Windows,
}

impl WorkflowPlatform {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "any" => Some(Self::Any),
            "unix" => Some(Self::Unix),
            "macos" => Some(Self::Macos),
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }

    /// Label stored on a plan node. `any` is omitted.
    #[must_use]
    pub fn as_plan_label(self) -> Option<String> {
        match self {
            Self::Any => None,
            Self::Unix => Some("unix".to_owned()),
            Self::Macos => Some("macos".to_owned()),
            Self::Linux => Some("linux".to_owned()),
            Self::Windows => Some("windows".to_owned()),
        }
    }
}

/// Platform label of the process running this binary.
#[must_use]
pub fn host_platform_label() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unix"
    }
}

/// Whether a plan-node platform label may run on this host.
///
/// `unix` matches macOS and Linux. An unknown label does not match.
#[must_use]
pub fn platform_matches_host(platform: &str) -> bool {
    match platform {
        "any" => true,
        "unix" => !cfg!(windows),
        "macos" => cfg!(target_os = "macos"),
        "linux" => cfg!(target_os = "linux"),
        "windows" => cfg!(windows),
        _ => false,
    }
}

/// Why `node` must not run on this host, if it must be skipped.
#[must_use]
pub fn host_skip_reason(
    platform: Option<&str>,
    architecture: Option<&str>,
    remote: bool,
) -> Option<String> {
    if remote {
        return None;
    }
    if let Some(platform) = platform
        && !platform_matches_host(platform)
    {
        return Some(format!(
            "platform `{platform}` does not match host `{}`",
            host_platform_label()
        ));
    }
    if let Some(architecture) = architecture
        && architecture != std::env::consts::ARCH
    {
        return Some(format!(
            "architecture `{architecture}` does not match host `{}`",
            std::env::consts::ARCH
        ));
    }
    None
}

/// One named workflow task after executor desugaring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowTask {
    pub depends_on: Vec<String>,
    pub platform: WorkflowPlatform,
    pub architecture: Option<String>,
    pub resources: NodeResources,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    pub artifacts: Vec<String>,
    /// Named outputs this task restores from dependencies before it runs.
    pub inputs: Vec<String>,
    pub operation: Operation,
    /// Checkout member that owns this task. `None` is the invoking checkout.
    pub source_member: Option<String>,
    /// `true` when the executor is [`Operation::Remote`].
    pub remote_eligible: bool,
}

/// Parsed `.nxr/workflows.toml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDocument {
    pub schema_version: u32,
    pub tasks: BTreeMap<String, WorkflowTask>,
}

/// Errors while loading or planning an optional workflow file.
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("workflow file {path} was not valid TOML: {message}")]
    Parse { path: String, message: String },
    #[error(
        "unsupported workflows schema version {found} in {path}; expected major version {expected}"
    )]
    UnsupportedVersion {
        path: String,
        found: u32,
        expected: u32,
    },
    #[error("invalid workflows file {path}: {message}")]
    Invalid { path: String, message: String },
    #[error("workflow selector is missing a task name")]
    EmptySelector,
    #[error("optional workflow file {path} is not present")]
    MissingFile { path: String },
    #[error(transparent)]
    Plan(#[from] GraphError),
}

impl WorkflowError {
    /// Existing public exit code. This does not add a new process status.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;
        match self {
            Self::Io { .. }
            | Self::Parse { .. }
            | Self::UnsupportedVersion { .. }
            | Self::Invalid { .. } => exit::INVALID_METADATA,
            Self::EmptySelector => exit::USAGE,
            Self::MissingFile { .. } => exit::NOT_FOUND,
            Self::Plan(error) => match error {
                GraphError::UnknownRoot { .. } => exit::NOT_FOUND,
                GraphError::MissingDependency { .. } | GraphError::Cycle { .. } => exit::TASK_GRAPH,
            },
        }
    }
}

/// Map `workflow` and `workflow:<task>` selectors.
///
/// Any other token, including a bare app or task name, returns [`None`].
/// Callers must leave flake resolution alone in that case.
#[must_use]
pub fn workflow_mode_from_selector(token: &str) -> Option<WorkflowPlanMode> {
    if token == "workflow" {
        return Some(WorkflowPlanMode::Explicit { root: None });
    }
    token
        .strip_prefix("workflow:")
        .map(|name| WorkflowPlanMode::Explicit {
            root: Some(name.to_owned()),
        })
}

/// Path of the optional workflow file under `repo_root`.
#[must_use]
pub fn workflow_path(repo_root: &Path) -> PathBuf {
    repo_root.join(WORKFLOWS_RELATIVE_PATH)
}

/// Load `.nxr/workflows.toml` when it exists.
///
/// A missing file returns `Ok(None)` and does not invent tasks.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the file exists but cannot be read or validated.
pub fn load_workflow_document(repo_root: &Path) -> Result<Option<WorkflowDocument>, WorkflowError> {
    let path = workflow_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|source| WorkflowError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_workflow_str(&text, &path.display().to_string()).map(Some)
}

/// Plan a workflow graph for `mode`.
///
/// [`WorkflowPlanMode::IfPresent`] returns `Ok(None)` when the file is absent.
/// [`WorkflowPlanMode::Explicit`] returns [`WorkflowError::MissingFile`] instead.
///
/// # Errors
///
/// Returns [`WorkflowError`] for a missing explicit file, an invalid document,
/// or a task-graph planning failure.
pub fn plan_workflows(
    repo_root: &Path,
    mode: WorkflowPlanMode,
    failure_policy: FailurePolicy,
    sink: Option<&mut dyn EventSink>,
) -> Result<Option<ExecutionPlan>, WorkflowError> {
    let Some(document) = load_workflow_document(repo_root)? else {
        return match mode {
            WorkflowPlanMode::IfPresent => Ok(None),
            WorkflowPlanMode::Explicit { .. } => Err(WorkflowError::MissingFile {
                path: workflow_path(repo_root).display().to_string(),
            }),
        };
    };
    plan_workflow_document(&document, &mode, failure_policy, sink).map(Some)
}

/// Plan `document` without touching the filesystem.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the selector or graph is invalid.
pub fn plan_workflow_document(
    document: &WorkflowDocument,
    mode: &WorkflowPlanMode,
    failure_policy: FailurePolicy,
    sink: Option<&mut dyn EventSink>,
) -> Result<ExecutionPlan, WorkflowError> {
    if document.tasks.is_empty() {
        return Err(WorkflowError::Invalid {
            path: WORKFLOWS_RELATIVE_PATH.to_owned(),
            message: "workflows document has no tasks".to_owned(),
        });
    }

    let all_names: Vec<String> = document.tasks.keys().cloned().collect();
    let single_root: Option<&str> = match mode {
        WorkflowPlanMode::Explicit { root: Some(name) } if name.is_empty() => {
            return Err(WorkflowError::EmptySelector);
        }
        WorkflowPlanMode::Explicit { root: Some(name) } => Some(name.as_str()),
        WorkflowPlanMode::IfPresent | WorkflowPlanMode::Explicit { root: None } => None,
    };
    let all_refs: Vec<&str> = all_names.iter().map(String::as_str).collect();
    let roots: Vec<&str> = match single_root {
        Some(name) => vec![name],
        None => all_refs,
    };

    let synthetic = synthetic_tasks(document);
    let serial_order = plan_serial_union(&synthetic, &roots)?;
    let graph = TaskGraph::subgraph_union(&synthetic, &roots)?;

    let nodes: Vec<PlanNode> = graph
        .node_ids()
        .filter_map(|id| {
            let task = document.tasks.get(id)?;
            Some(PlanNode {
                id: id.to_owned(),
                depends_on: graph
                    .dependencies(id)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default(),
                interactive: false,
                resources: task.resources.clone(),
                operation: Some(task.operation.clone()),
                platform: task.platform.as_plan_label(),
                architecture: task.architecture.clone(),
                environment: task.environment.clone(),
                working_directory: task.working_directory.clone(),
                artifacts: task.artifacts.clone(),
                inputs: task.inputs.clone(),
                artifact_reuse: false,
                cargo_output: false,
                source_member: task.source_member.clone(),
                remote_eligible: task.remote_eligible,
            })
        })
        .collect();

    let primary_root = roots
        .first()
        .map(|root| (*root).to_owned())
        .expect("empty documents are rejected");
    let extra_roots = if roots.len() > 1 {
        roots.iter().map(|root| (*root).to_owned()).collect()
    } else {
        Vec::new()
    };
    let waves = dependency_waves(&nodes);

    let plan = ExecutionPlan {
        schema_version: ExecutionPlan::SCHEMA_VERSION,
        root: primary_root,
        roots: extra_roots,
        failure_policy,
        argument_forwarding: ArgumentForwarding::Root,
        nodes,
        serial_order,
        waves,
    };

    if let Some(sink) = sink {
        sink.emit(Event::plan_created(
            plan.root.clone(),
            if plan.roots.is_empty() {
                None
            } else {
                Some(plan.roots.clone())
            },
            plan.nodes.len(),
        ));
    }

    Ok(plan)
}

/// Graph-only stand-in so workflow edges reuse the task planner.
///
/// The placeholder `app` is discarded. Plan nodes carry [`WorkflowTask::operation`].
fn synthetic_tasks(document: &WorkflowDocument) -> BTreeMap<String, TaskDefinition> {
    document
        .tasks
        .iter()
        .map(|(name, task)| {
            let mut definition = TaskDefinition::new("workflow");
            definition.depends_on.clone_from(&task.depends_on);
            (name.clone(), definition)
        })
        .collect()
}

fn parse_workflow_str(text: &str, path: &str) -> Result<WorkflowDocument, WorkflowError> {
    let file: WorkflowFile = toml::from_str(text).map_err(|error| WorkflowError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    if file.schema_version != WORKFLOWS_SCHEMA_VERSION {
        return Err(WorkflowError::UnsupportedVersion {
            path: path.to_owned(),
            found: file.schema_version,
            expected: WORKFLOWS_SCHEMA_VERSION,
        });
    }

    let source_members: BTreeMap<String, Option<String>> = file
        .tasks
        .iter()
        .map(|(name, task)| (name.clone(), task.source_member.clone()))
        .collect();

    let mut tasks = BTreeMap::new();
    for (name, task) in file.tasks {
        validate_task_name(&name).map_err(|message| WorkflowError::Invalid {
            path: path.to_owned(),
            message,
        })?;
        let platform = match task.platform.as_deref() {
            None => WorkflowPlatform::Any,
            Some(raw) => WorkflowPlatform::parse(raw).ok_or_else(|| WorkflowError::Invalid {
                path: path.to_owned(),
                message: format!(
                    "task `{name}` platform `{raw}` is not any, unix, macos, linux, or windows"
                ),
            })?,
        };
        if let Some(architecture) = task.architecture.as_deref() {
            validate_token(&format!("task `{name}` architecture"), architecture).map_err(
                |message| WorkflowError::Invalid {
                    path: path.to_owned(),
                    message,
                },
            )?;
        }
        validate_relative_paths(&name, task.working_directory.as_deref(), &task.artifacts)
            .map_err(|message| WorkflowError::Invalid {
                path: path.to_owned(),
                message,
            })?;
        let operation = operation_from_executor(&name, task.executor).map_err(|message| {
            WorkflowError::Invalid {
                path: path.to_owned(),
                message,
            }
        })?;
        let remote_eligible = matches!(operation, Operation::Remote { .. });
        let depends_on = task
            .depends_on
            .iter()
            .map(|edge| {
                resolve_workflow_dependency(edge, &name, &source_members, path).map_err(|message| {
                    WorkflowError::Invalid {
                        path: path.to_owned(),
                        message,
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let resources = task
            .resources
            .as_ref()
            .map(NodeResources::from_task_resources)
            .unwrap_or_default();
        for input in &task.inputs {
            if !depends_on.iter().any(|edge| edge == input) {
                return Err(WorkflowError::Invalid {
                    path: path.to_owned(),
                    message: format!("task `{name}` input `{input}` is not a dependency"),
                });
            }
        }
        tasks.insert(
            name,
            WorkflowTask {
                depends_on,
                platform,
                architecture: task.architecture,
                resources,
                environment: task.env,
                working_directory: task.working_directory,
                artifacts: task.artifacts,
                inputs: task.inputs.clone(),
                operation,
                source_member: task.source_member,
                remote_eligible,
            },
        );
    }

    Ok(WorkflowDocument {
        schema_version: file.schema_version,
        tasks,
    })
}

/// Task keys whose [`WorkflowTask::source_member`] matches `member`, plus transitive dependents.
#[must_use]
pub fn invalidated_by_member(document: &WorkflowDocument, member: &str) -> BTreeSet<String> {
    let seeds: Vec<String> = document
        .tasks
        .iter()
        .filter(|(_, task)| task.source_member.as_deref() == Some(member))
        .map(|(name, _)| name.clone())
        .collect();
    if seeds.is_empty() {
        return BTreeSet::new();
    }

    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, task) in &document.tasks {
        for dep in &task.depends_on {
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(name.as_str());
        }
    }

    let mut invalidated = BTreeSet::new();
    let mut stack = seeds.iter().map(String::as_str).collect::<Vec<_>>();
    while let Some(current) = stack.pop() {
        if !invalidated.insert(current.to_owned()) {
            continue;
        }
        if let Some(children) = dependents.get(current) {
            for child in children {
                stack.push(child);
            }
        }
    }
    invalidated
}

fn resolve_workflow_dependency(
    edge: &str,
    task_name: &str,
    source_members: &BTreeMap<String, Option<String>>,
    path: &str,
) -> Result<String, String> {
    // Prefer an exact task key, including keys that contain `:`.
    if source_members.contains_key(edge) {
        return Ok(edge.to_owned());
    }
    if let Some((member, dep_task)) = edge.split_once(':') {
        if member.is_empty() || dep_task.is_empty() {
            return Err(format!(
                "task `{task_name}` dependency `{edge}` in {path} is invalid"
            ));
        }
        let mut matched = None;
        for (key, source_member) in source_members {
            if source_member.as_deref() == Some(member) && key == dep_task {
                if matched.is_some() {
                    return Err(format!(
                        "task `{task_name}` dependency `{edge}` in {path} is ambiguous"
                    ));
                }
                matched = Some(key.clone());
            }
        }
        matched
            .ok_or_else(|| format!("task `{task_name}` dependency `{edge}` in {path} is unknown"))
    } else {
        Ok(edge.to_owned())
    }
}

fn validate_task_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.chars().any(char::is_whitespace)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(format!("invalid workflow task name `{name}`"));
    }
    Ok(())
}

fn operation_from_executor(
    task: &str,
    executor: WorkflowExecutorFile,
) -> Result<Operation, String> {
    let selected = [
        executor.flake.is_some(),
        executor.native.is_some(),
        executor.winbuild.is_some(),
        executor.remote.is_some(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();
    if selected != 1 {
        return Err(format!(
            "task `{task}` executor must set exactly one of flake, native, winbuild, or remote"
        ));
    }
    if let Some(app) = executor.flake {
        validate_token(&format!("task `{task}` flake app"), &app)?;
        return Ok(Operation::FlakeApp { name: app });
    }
    if let Some(argv) = executor.native {
        if argv.is_empty() || argv.iter().any(String::is_empty) {
            return Err(format!(
                "task `{task}` native argv must be a non-empty argument list"
            ));
        }
        return Ok(Operation::NativeCommand { argv });
    }
    if let Some(winbuild) = executor.winbuild {
        let (task_id, manifest) = match winbuild {
            WorkflowWinbuildFile::Id(task_id) => (task_id, ".winbuild.json".to_owned()),
            WorkflowWinbuildFile::Spec { task, manifest } => (
                task,
                manifest.unwrap_or_else(|| ".winbuild.json".to_owned()),
            ),
        };
        validate_token(&format!("task `{task}` winbuild task id"), &task_id)?;
        reject_escaping_path(&format!("task `{task}` winbuild manifest"), &manifest)?;
        return Ok(Operation::WinBuildTask { task_id, manifest });
    }
    if let Some(remote) = executor.remote {
        validate_token(&format!("task `{task}` remote target"), &remote.target)?;
        validate_token(&format!("task `{task}` remote task"), &remote.task)?;
        if remote.source_url.is_empty() || remote.source_url.chars().any(char::is_whitespace) {
            return Err(format!("task `{task}` remote source_url is required"));
        }
        let release_revision = !remote.source_revision.is_empty();
        let release_lock = !remote.lock_digest.is_empty();
        if release_revision && !remote.source_ref.is_empty() {
            return Err(format!(
                "task `{task}` remote sets source_revision and source_ref"
            ));
        }
        if release_lock && !remote.lock_file.is_empty() {
            return Err(format!(
                "task `{task}` remote sets lock_digest and lock_file"
            ));
        }
        if release_revision {
            if !is_git_revision(&remote.source_revision) {
                return Err(format!(
                    "task `{task}` remote source_revision must be 40 or 64 lowercase hex characters"
                ));
            }
        } else if remote.source_ref.is_empty()
            || remote.source_ref.chars().any(char::is_whitespace)
            || remote.source_ref.contains("..")
        {
            return Err(format!(
                "task `{task}` remote requires source_revision or source_ref"
            ));
        }
        if release_lock {
            if !is_sha256_digest(&remote.lock_digest) {
                return Err(format!(
                    "task `{task}` remote lock_digest must be sha256 and 64 hex characters"
                ));
            }
        } else {
            reject_escaping_path(
                &format!("task `{task}` remote lock_file"),
                &remote.lock_file,
            )?;
        }
        if !remote.build_root.is_empty() {
            reject_escaping_path(
                &format!("task `{task}` remote build_root"),
                &remote.build_root,
            )?;
        }
        if !remote.manifest.is_empty() {
            reject_escaping_path(&format!("task `{task}` remote manifest"), &remote.manifest)?;
        }
        return Ok(Operation::Remote {
            target: remote.target,
            task: remote.task,
            source_url: remote.source_url,
            source_revision: remote.source_revision,
            lock_digest: remote.lock_digest,
            source_ref: remote.source_ref,
            lock_file: remote.lock_file,
            build_root: remote.build_root,
            manifest: remote.manifest,
        });
    }
    Err(format!("task `{task}` executor is missing"))
}

fn validate_relative_paths(
    task: &str,
    working_directory: Option<&str>,
    artifacts: &[String],
) -> Result<(), String> {
    if let Some(directory) = working_directory {
        reject_escaping_path(&format!("task `{task}` working_directory"), directory)?;
    }
    for artifact in artifacts {
        reject_escaping_path(&format!("task `{task}` artifact"), artifact)?;
    }
    Ok(())
}

fn reject_escaping_path(label: &str, value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() || value.contains("..") || value.contains('\\') {
        return Err(format!(
            "{label} must be a relative path inside the workflow root"
        ));
    }
    Ok(())
}

/// Group nodes that share no unfinished dependency into the same wave.
fn dependency_waves(nodes: &[PlanNode]) -> Vec<Vec<String>> {
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in nodes {
        indegree.insert(node.id.as_str(), node.depends_on.len());
        for dependency in &node.depends_on {
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(node.id.as_str());
        }
    }

    let mut waves = Vec::new();
    let mut remaining = nodes.len();
    while remaining > 0 {
        let mut wave: Vec<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| (*id).to_owned())
            .collect();
        wave.sort();
        if wave.is_empty() {
            break;
        }
        for id in &wave {
            indegree.remove(id.as_str());
            remaining -= 1;
            if let Some(children) = dependents.get(id.as_str()) {
                for child in children {
                    if let Some(degree) = indegree.get_mut(child) {
                        *degree = degree.saturating_sub(1);
                    }
                }
            }
        }
        waves.push(wave);
    }
    waves
}

/// Walk upward from `start` until `.nxr/workflows.toml` exists.
#[must_use]
pub fn find_workflow_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if workflow_path(&current).is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn validate_token(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{label} must be a single token"));
    }
    Ok(())
}

fn is_sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Fill development remote pins from the coordinator checkout.
///
/// `source_ref` becomes one commit. `lock_file` becomes that file's WinNix
/// digest, or the sha256 of its bytes when the document has no digest field.
/// The workflow file is not rewritten. Submodule gitlinks are read from the
/// resolved commit and are not resolved again on the worker.
///
/// # Errors
///
/// Returns an error when git cannot resolve `source_ref` or the lock file is
/// missing.
pub fn resolve_plan_pins(
    root: &Path,
    plan: &mut crate::plan_exec::ExecutionPlan,
) -> Result<Vec<serde_json::Value>, String> {
    let mut manifests = Vec::new();
    for node in &mut plan.nodes {
        let Some(Operation::Remote {
            target,
            task,
            source_url,
            source_revision,
            lock_digest,
            source_ref,
            lock_file,
            build_root,
            manifest,
        }) = node.operation.clone()
        else {
            continue;
        };
        let revision = if source_revision.is_empty() {
            git_commit(root, &source_ref)?
        } else {
            source_revision.clone()
        };
        let digest = if lock_digest.is_empty() {
            lock_digest_from_file(root, &lock_file)?
        } else {
            lock_digest.clone()
        };
        let submodules = submodule_gitlinks(root, &revision).unwrap_or_default();
        node.operation = Some(Operation::Remote {
            target: target.clone(),
            task: task.clone(),
            source_url: source_url.clone(),
            source_revision: revision.clone(),
            lock_digest: digest.clone(),
            source_ref: String::new(),
            lock_file: String::new(),
            build_root: build_root.clone(),
            manifest: manifest.clone(),
        });
        manifests.push(serde_json::json!({
            "task": node.id,
            "target": target,
            "winbuild_task": task,
            "source_url": source_url,
            "source_revision": revision,
            "submodules": submodules,
            "lock_digest": digest,
            "build_root": build_root,
            "manifest": manifest,
        }));
    }
    Ok(manifests)
}

fn git_commit(root: &Path, reference: &str) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
        .output()
        .map_err(|error| format!("git rev-parse: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git could not resolve `{reference}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !is_git_revision(&revision) {
        return Err(format!("git resolved `{reference}` to `{revision}`"));
    }
    Ok(revision)
}

fn submodule_gitlinks(root: &Path, revision: &str) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", revision])
        .output()
        .map_err(|error| format!("git ls-tree: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut links = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let mode = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        let sha = parts.next().unwrap_or("");
        let path = line.split('\t').nth(1).unwrap_or("");
        if mode == "160000" && kind == "commit" && !path.is_empty() {
            links.push(format!("{path} {sha}"));
        }
    }
    Ok(links)
}

fn lock_digest_from_file(root: &Path, relative: &str) -> Result<String, String> {
    reject_escaping_path("lock file", relative)?;
    let path = root.join(relative);
    let bytes = fs::read(&path).map_err(|error| format!("read lock file: {error}"))?;
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(digest) = value.get("digest").and_then(|item| item.as_str())
        && is_sha256_digest(digest)
    {
        return Ok(digest.to_owned());
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowFile {
    schema_version: u32,
    #[serde(default)]
    tasks: BTreeMap<String, WorkflowTaskFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowTaskFile {
    #[serde(default)]
    source_member: Option<String>,
    #[serde(default, alias = "dependsOn")]
    depends_on: Vec<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    resources: Option<TaskResources>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default)]
    artifacts: Vec<String>,
    /// Dependency task names whose stored outputs this task consumes.
    #[serde(default)]
    inputs: Vec<String>,
    executor: WorkflowExecutorFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowExecutorFile {
    #[serde(default)]
    flake: Option<String>,
    #[serde(default)]
    native: Option<Vec<String>>,
    #[serde(default)]
    winbuild: Option<WorkflowWinbuildFile>,
    #[serde(default)]
    remote: Option<WorkflowRemoteFile>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WorkflowWinbuildFile {
    Id(String),
    Spec {
        task: String,
        #[serde(default)]
        manifest: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowRemoteFile {
    target: String,
    task: String,
    source_url: String,
    #[serde(default)]
    source_revision: String,
    #[serde(default)]
    lock_digest: String,
    #[serde(default)]
    source_ref: String,
    #[serde(default)]
    lock_file: String,
    #[serde(default)]
    build_root: String,
    #[serde(default)]
    manifest: String,
}

/// Where one remote workflow leaf is placed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteLeafPath {
    /// Linux leaf: `submit_execution`, then fetch each declared artifact.
    LinuxAttempt,
    /// Existing `nixplane native-build` command.
    NativeBuild,
}

/// Linux remote leaves use the execution contract. Every other remote leaf
/// stays on the native-build command.
#[must_use]
pub fn remote_leaf_path(node: &PlanNode) -> RemoteLeafPath {
    if node.platform.as_deref() == Some("linux")
        && matches!(node.operation, Some(Operation::Remote { .. }))
    {
        RemoteLeafPath::LinuxAttempt
    } else {
        RemoteLeafPath::NativeBuild
    }
}

/// Submit body for one Linux workflow leaf.
///
/// The object has two fields:
/// - `spec`: the execution spec for this leaf. `outputs` lists only
///   `node.artifacts`.
/// - `source_set`: the leaf source set passed by the caller.
///
/// The body has no dirty manifest, no source locator, and no sibling member
/// list. Preparation and verification paths are not added.
///
/// # Errors
///
/// Returns an error when the node has no operation or the spec cannot be built.
pub fn linux_leaf_submit_body(
    node: &PlanNode,
    source: &SourceSetV1,
    workflow_digest: &str,
) -> Result<Value, String> {
    let task = workflow_task_from_node(node)?;
    let spec =
        crate::execution_spec::execution_spec_for_task(source, workflow_digest, &node.id, &task)?;
    let spec_value = serde_json::to_value(&spec).map_err(|err| format!("execution spec: {err}"))?;
    let source_value = serde_json::to_value(source).map_err(|err| format!("source set: {err}"))?;
    Ok(serde_json::json!({
        "spec": spec_value,
        "source_set": source_value,
    }))
}

/// Submit one Linux leaf, then fetch each declared artifact under `checkout`.
///
/// A failed submit returns before any fetch. `fetch` is `NixplaneCliTransport::fetch_output`
/// in the CLI and a recording writer in tests. The artifact `name` is the
/// declared name; the file is `checkout.join(name)`.
///
/// # Errors
///
/// Returns an error when the node is not a Linux remote leaf, the spec cannot
/// be built, submit fails, the attempt id is missing, or a fetch fails.
pub fn place_linux_leaf<T, F>(
    node: &PlanNode,
    checkout: &Path,
    source: &SourceSetV1,
    workflow_digest: &str,
    transport: &T,
    mut fetch: F,
) -> Result<String, String>
where
    T: ExecutionTransport,
    F: FnMut(&T, &str, &str, &Path) -> Result<String, String>,
{
    if remote_leaf_path(node) != RemoteLeafPath::LinuxAttempt {
        return Err("only a linux remote leaf is placed on the execution contract".to_owned());
    }
    let body = linux_leaf_submit_body(node, source, workflow_digest)?;
    let response = submit_execution(transport, &body)?;
    let attempt_id = response
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "submit did not return an attempt id".to_owned())?
        .to_owned();
    for name in &node.artifacts {
        let destination = artifact_destination(checkout, name)?;
        if let Some(parent) = destination.parent()
            && parent != checkout
        {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create artifact directory {}: {err}", parent.display()))?;
        }
        fetch(transport, &attempt_id, name, &destination)?;
    }
    Ok(attempt_id)
}

fn workflow_task_from_node(node: &PlanNode) -> Result<WorkflowTask, String> {
    let operation = node
        .operation
        .clone()
        .ok_or_else(|| format!("task `{}` has no operation", node.id))?;
    let platform = match node.platform.as_deref() {
        None | Some("any") => WorkflowPlatform::Any,
        Some("unix") => WorkflowPlatform::Unix,
        Some("macos") => WorkflowPlatform::Macos,
        Some("linux") => WorkflowPlatform::Linux,
        Some("windows") => WorkflowPlatform::Windows,
        Some(other) => {
            return Err(format!("task `{}` platform `{other}` is unknown", node.id));
        }
    };
    Ok(WorkflowTask {
        depends_on: node.depends_on.clone(),
        platform,
        architecture: node.architecture.clone(),
        resources: node.resources.clone(),
        environment: node.environment.clone(),
        working_directory: node.working_directory.clone(),
        artifacts: node.artifacts.clone(),
        inputs: node.inputs.clone(),
        operation,
        source_member: node.source_member.clone(),
        remote_eligible: node.remote_eligible,
    })
}

fn artifact_destination(checkout: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("artifact `{name}` escapes the leaf checkout"));
    }
    Ok(checkout.join(name))
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    use std::path::Path;
    use std::path::PathBuf;
    #[cfg(not(windows))]
    use std::sync::Mutex;

    use nxr_core::Operation;
    use nxr_core::diagnostics::exit;
    #[cfg(not(windows))]
    use serde_json::{Value, json};

    use super::*;
    use crate::events::{RecordingSink, event_kind};
    use crate::execute::{
        ExecContext, ExecuteError, run_operation_plan, run_operation_plan_callbacks,
    };
    #[cfg(not(windows))]
    use crate::execute::{ExecutorName, OperationResult, OperationRun};
    #[cfg(not(windows))]
    use crate::identity::SourceSetV1;
    #[cfg(not(windows))]
    use crate::remote_execution::ExecutionTransport;

    #[cfg(not(windows))]
    struct RecordingTransport {
        submits: Mutex<Vec<Value>>,
        fail_submit: bool,
    }

    #[cfg(not(windows))]
    impl ExecutionTransport for RecordingTransport {
        fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
            if method != "submit" {
                return Err(format!("unexpected execution method `{method}`"));
            }
            self.submits.lock().expect("submits").push(body.clone());
            if self.fail_submit {
                return Err("submit refused".to_owned());
            }
            Ok(json!({ "attempt_id": "att_linux_1", "accepted": true }))
        }
    }

    #[cfg(unix)]
    struct FourNodeRun {
        root: tempfile::TempDir,
        result: Result<OperationRun, ExecuteError>,
        submits: Vec<Value>,
        fetches: Vec<(String, String)>,
    }

    #[cfg(unix)]
    fn four_node_document() -> WorkflowDocument {
        parse(
            r#"
schema_version = 1

[tasks.prep-a]
executor = { native = ["/bin/sh", "-c", "mkdir -p preparation && printf '%s' 'alpha-prep-bytes' > preparation/alpha.txt"] }

[tasks.prep-b]
executor = { native = ["/bin/sh", "-c", "mkdir -p preparation && printf '%s' 'beta-prep-bytes' > preparation/beta.txt"] }

[tasks.linux-test]
platform = "linux"
depends_on = ["prep-a", "prep-b"]
artifacts = ["linux-result.txt"]
executor = { remote = { target = "linux-worker", task = "test", source_url = "https://example.invalid/repo.git", source_revision = "0123456789abcdef0123456789abcdef01234567", lock_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }

[tasks.verify]
depends_on = ["linux-test"]
executor = { native = ["/bin/sh", "-c", "mkdir -p verification && cp linux-result.txt verification/consumer.txt"] }
"#,
        )
    }

    #[cfg(unix)]
    fn run_four_node(fail_submit: bool) -> FourNodeRun {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("sibling-workspace")).expect("sibling");
        std::fs::write(
            root.path().join("sibling-workspace/upload.txt"),
            "sibling-upload-bytes",
        )
        .expect("sibling bytes");
        let document = four_node_document();
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("verify".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut remote_ids: Vec<_> = plan
            .nodes
            .iter()
            .filter(|node| node.remote_eligible)
            .map(|node| node.id.as_str())
            .collect();
        remote_ids.sort_unstable();
        assert_eq!(plan.nodes.len(), 4);
        assert_eq!(remote_ids, ["linux-test"]);
        let transport = RecordingTransport {
            submits: Mutex::new(Vec::new()),
            fail_submit,
        };
        let source = SourceSetV1::fixture("linux-leaf");
        let fetches = Mutex::new(Vec::new());
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let result = run_operation_plan_callbacks(
            &plan,
            &ctx,
            2,
            None,
            None,
            Some(&mut |_operation, ctx, node| {
                let checkout = ctx.cwd.clone().expect("checkout");
                match remote_leaf_path(node) {
                    RemoteLeafPath::NativeBuild => Err(ExecuteError::UnavailableExecutor {
                        executor: ExecutorName::Remote,
                        detail: "linux graph reached native-build".to_owned(),
                    }),
                    RemoteLeafPath::LinuxAttempt => {
                        place_linux_leaf(
                            node,
                            &checkout,
                            &source,
                            "wf_linux_test",
                            &transport,
                            |_transport, attempt, name, out| {
                                fetches
                                    .lock()
                                    .expect("fetches")
                                    .push((attempt.to_owned(), name.to_owned()));
                                std::fs::write(out, "returned-linux-artifact")
                                    .map_err(|err| format!("write artifact: {err}"))?;
                                Ok("returned-linux-artifact".to_owned())
                            },
                        )
                        .map_err(|detail| {
                            ExecuteError::UnavailableExecutor {
                                executor: ExecutorName::Remote,
                                detail,
                            }
                        })?;
                        Ok(OperationResult::Exited {
                            code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                        })
                    }
                }
            }),
        );
        let submits = transport.submits.lock().expect("submits").clone();
        let fetches = fetches.lock().expect("fetches").clone();
        FourNodeRun {
            root,
            result,
            submits,
            fetches,
        }
    }

    #[cfg(unix)]
    fn assert_linux_submit_body(body: &Value) {
        let mut keys = body
            .as_object()
            .expect("body")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, ["source_set".to_owned(), "spec".to_owned()]);
        assert_eq!(body["spec"]["task_id"], "linux-test");
        assert_eq!(body["spec"]["platform"], "linux");
        assert_eq!(body["spec"]["workflow_definition_digest"], "wf_linux_test");
        assert_eq!(
            body["spec"]["outputs"],
            json!([{ "name": "linux-result.txt" }])
        );
        assert_eq!(body["spec"]["leaf"]["kind"], "remote");
        assert!(body["spec"]["leaf"].get("argv").is_none());
        assert_eq!(body["source_set"]["schema_version"], "source-set-v1");
        assert_eq!(body["source_set"]["kind"], "fixture");
        assert_eq!(body["source_set"]["commit"], "linux-leaf");
        assert!(body["source_set"].get("members").is_none());
        assert!(body["source_set"].get("files").is_none());
        assert!(body["source_set"].get("dirty_snapshot_id").is_none());
        assert!(body.get("dirty_manifest").is_none());
        assert!(body.get("source_locator").is_none());
        let text = body.to_string();
        for absent in [
            "preparation",
            "verification",
            "alpha-prep-bytes",
            "beta-prep-bytes",
            "sibling-workspace",
            "sibling-upload-bytes",
            "prep-a",
            "prep-b",
            "verify",
        ] {
            assert!(
                !text.contains(absent),
                "{absent} leaked into the submit body"
            );
        }
    }

    #[cfg(not(windows))]
    fn assert_windows_native_build(
        seen: Option<(String, String, String, String, String, String, String)>,
    ) {
        let (target, task, source_url, source_revision, lock_digest, build_root, manifest) =
            seen.expect("native-build shape");
        assert_eq!(target, "windows-pc");
        assert_eq!(task, "fixture");
        assert_eq!(source_url, "https://example.invalid/repo.git");
        assert_eq!(source_revision, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(
            lock_digest,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(build_root.is_empty());
        assert!(manifest.is_empty());
    }

    fn parse(text: &str) -> WorkflowDocument {
        parse_workflow_str(text, "workflows.toml").expect("parse workflows")
    }

    #[test]
    fn bare_names_are_not_workflow_selectors() {
        for token in ["ci", "fmt", "test", "build", "app:hello", "task:ci"] {
            assert_eq!(workflow_mode_from_selector(token), None, "{token}");
        }
        assert_eq!(
            workflow_mode_from_selector("workflow"),
            Some(WorkflowPlanMode::Explicit { root: None })
        );
        assert_eq!(
            workflow_mode_from_selector("workflow:finish"),
            Some(WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned())
            })
        );
    }

    #[test]
    fn missing_file_does_not_invent_a_graph() {
        let root = tempfile::tempdir().expect("tempdir");
        let loaded = load_workflow_document(root.path()).expect("load");
        assert!(loaded.is_none());
        let planned = plan_workflows(
            root.path(),
            WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect("if present");
        assert!(planned.is_none());

        let explicit = plan_workflows(
            root.path(),
            WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("explicit missing file");
        assert!(matches!(explicit, WorkflowError::MissingFile { .. }));
        assert_eq!(explicit.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn rejects_inline_script_bodies() {
        let text = r#"
schema_version = 1

[tasks.prep]
script = "echo copied-build-script"
executor = { native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(text, "workflows.toml").expect_err("script field");
        assert!(matches!(error, WorkflowError::Parse { .. }));
        assert_eq!(error.exit_code(), exit::INVALID_METADATA);
    }

    #[test]
    fn rejects_two_executors_and_unknown_platform() {
        let both = r#"
schema_version = 1

[tasks.prep]
executor = { flake = "fmt", native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(both, "workflows.toml").expect_err("two executors");
        assert!(matches!(error, WorkflowError::Invalid { .. }));

        let platform = r#"
schema_version = 1

[tasks.prep]
platform = "plan9"
executor = { native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(platform, "workflows.toml").expect_err("platform");
        assert!(matches!(error, WorkflowError::Invalid { .. }));
    }

    #[test]
    fn winbuild_reference_plans_on_any_host() {
        let document = parse(
            r#"
schema_version = 1

[tasks.win]
platform = "windows"
dependsOn = []
executor = { winbuild = "link-release" }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].platform.as_deref(), Some("windows"));
        assert_eq!(
            plan.nodes[0].operation,
            Some(Operation::WinBuildTask {
                task_id: "link-release".to_owned(),
                manifest: ".winbuild.json".to_owned(),
            })
        );
    }

    #[test]
    fn cycle_maps_to_task_graph_exit() {
        let document = parse(
            r#"
schema_version = 1

[tasks.a]
depends_on = ["b"]
executor = { native = ["/bin/true"] }

[tasks.b]
depends_on = ["a"]
executor = { native = ["/bin/true"] }
"#,
        );
        let error = plan_workflow_document(
            &document,
            &WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("cycle");
        assert!(matches!(
            error,
            WorkflowError::Plan(GraphError::Cycle { .. })
        ));
        assert_eq!(error.exit_code(), exit::TASK_GRAPH);
    }

    #[test]
    fn native_workflow_fixture_plans_and_runs_without_nix() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/native-workflow");
        let mut sink = RecordingSink::new();
        let plan = plan_workflows(
            &fixture,
            WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned()),
            },
            FailurePolicy::FailFast,
            Some(&mut sink),
        )
        .expect("plan fixture")
        .expect("file present");

        assert_eq!(plan.root, "finish");
        assert!(plan.roots.is_empty());
        assert_eq!(
            plan.serial_order,
            vec!["prep".to_owned(), "mid".to_owned(), "finish".to_owned()]
        );
        assert_eq!(
            sink.events(),
            &[Event::plan_created("finish".to_owned(), None, 3)]
        );
        assert_eq!(event_kind(&sink.events()[0]), "plan_created");

        let ids: Vec<_> = plan.node_ids().collect();
        assert_eq!(ids, vec!["finish", "mid", "prep"]);
        assert!(plan.nodes.iter().all(|node| {
            matches!(node.operation, Some(Operation::NativeCommand { .. }))
                && node.platform.as_deref() == Some("unix")
        }));

        #[cfg(unix)]
        {
            let trap_dir = tempfile::tempdir().expect("trap dir");
            let nix_marker = trap_dir.path().join("nix-ran");
            let wsl_marker = trap_dir.path().join("wsl-ran");
            write_trap(trap_dir.path(), "nix", &nix_marker);
            write_trap(trap_dir.path(), "wsl", &wsl_marker);
            write_fixture_echo(trap_dir.path());
            let ctx = ExecContext {
                path: trap_dir.path().display().to_string(),
                nix_override: Some("/definitely/missing/nix".to_owned()),
                cwd: None,
                env: BTreeMap::new(),
                member_roots: BTreeMap::new(),
            };
            let mut run_sink = RecordingSink::new();
            let run = run_operation_plan(&plan, &ctx, 1, Some(&mut run_sink)).expect("run native");
            assert_eq!(run.exit_code, 0);
            assert_eq!(
                run.order,
                vec!["prep".to_owned(), "mid".to_owned(), "finish".to_owned()]
            );
            assert_eq!(run.stdout, "prep\nmid\nfinish\n");
            assert!(!nix_marker.exists(), "native graph must not spawn nix");
            assert!(!wsl_marker.exists(), "native graph must not start WSL");
            for event in run_sink.events() {
                let kind = event_kind(event);
                assert!(
                    matches!(
                        kind,
                        "node_queued"
                            | "node_started"
                            | "stdout_chunk"
                            | "node_exited"
                            | "run_completed"
                            | "diagnostic"
                    ),
                    "unexpected event {kind}"
                );
                let encoded = serde_json::to_value(event).expect("serialize event");
                let restored: Event = serde_json::from_value(encoded).expect("deserialize event");
                assert_eq!(event_kind(&restored), kind);
            }
        }
    }

    #[test]
    fn missing_native_program_fails_instead_of_skipping() {
        let document = parse(
            r#"
schema_version = 1

[tasks.only]
executor = { native = ["nxr-fixture-missing", "nope"] }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("only".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let error = run_operation_plan(&plan, &ExecContext::isolated(""), 1, None)
            .expect_err("absent program must fail the run");
        assert!(
            matches!(error, ExecuteError::Spawn { .. }),
            "expected spawn failure, got {error}"
        );
        let detail = error.to_string();
        assert!(
            detail.contains("not found"),
            "spawn failure should name the missing program, got {detail}"
        );
        assert!(
            !detail.contains("skipped"),
            "a missing program must not be reported as a skip: {detail}"
        );
        assert_eq!(error.exit_code(), exit::PROCESS_SUPERVISION);
    }

    #[cfg(unix)]
    fn write_trap(dir: &std::path::Path, name: &str, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        std::fs::write(&program, script).expect("write trap");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod trap");
    }

    #[cfg(unix)]
    fn write_fixture_echo(dir: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("nxr-fixture-echo");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s\\n' \"$1\"\n").expect("write echo");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod echo");
    }

    #[test]
    fn flake_reference_is_a_flake_app_operation() {
        let document = parse(
            r#"
schema_version = 1

[tasks.fmt]
executor = { flake = "fmt" }
"#,
        );
        assert_eq!(
            document.tasks["fmt"].operation,
            Operation::FlakeApp {
                name: "fmt".to_owned()
            }
        );
    }

    #[test]
    fn explicit_unknown_root_is_not_found() {
        let document = parse(
            r#"
schema_version = 1

[tasks.prep]
executor = { native = ["/bin/true"] }
"#,
        );
        let error = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("missing".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("unknown root");
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn independent_tasks_share_a_wave_and_keep_resource_locks() {
        let document = parse(
            r#"
schema_version = 1

[tasks.left]
platform = "macos"
architecture = "aarch64"
executor = { native = ["/bin/echo", "left"] }

[tasks.left.resources]
exclusive = ["cargo-target"]

[tasks.right]
platform = "linux"
executor = { native = ["/bin/echo", "right"] }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit { root: None },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert_eq!(plan.waves.len(), 1);
        assert_eq!(plan.waves[0], vec!["left".to_owned(), "right".to_owned()]);
        let left = plan.nodes.iter().find(|node| node.id == "left").unwrap();
        assert_eq!(left.resources.exclusive, vec!["cargo-target".to_owned()]);
        assert_eq!(left.platform.as_deref(), Some("macos"));
        assert_eq!(left.architecture.as_deref(), Some("aarch64"));
        assert_eq!(
            plan.nodes
                .iter()
                .find(|node| node.id == "right")
                .unwrap()
                .platform
                .as_deref(),
            Some("linux")
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_dependency_does_not_start_its_dependent() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join(".nxr")).expect("nxr dir");
        std::fs::write(
            root.path().join(".nxr/workflows.toml"),
            r#"
schema_version = 1

[tasks.bad]
executor = { native = ["/bin/sh", "-c", "exit 7"] }

[tasks.next]
depends_on = ["bad"]
executor = { native = ["/bin/echo", "should-not-run"] }
"#,
        )
        .expect("write workflow");
        let plan = plan_workflows(
            root.path(),
            WorkflowPlanMode::Explicit {
                root: Some("next".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan")
        .expect("present");
        let ctx = ExecContext::isolated("");
        let run = run_operation_plan(&plan, &ctx, 2, None).expect("run");
        assert_eq!(run.exit_code, 7);
        assert_eq!(run.order, vec!["bad".to_owned()]);
        assert!(!run.stdout.contains("should-not-run"));
    }

    #[cfg(unix)]
    #[test]
    fn native_task_receives_directory_environment_and_artifact_hash() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("work")).expect("work");
        let document = parse(
            r#"
schema_version = 1

[tasks.write]
working_directory = "work"
artifacts = ["out.txt"]
executor = { native = ["/bin/sh", "-c", "printf '%s' \"$NXR_WF_PROBE\" > out.txt"] }

[tasks.write.env]
NXR_WF_PROBE = "from-env"
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("write".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut sink = RecordingSink::new();
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let run = run_operation_plan(&plan, &ctx, 1, Some(&mut sink)).expect("run");
        assert_eq!(run.exit_code, 0);
        let body = std::fs::read_to_string(root.path().join("work/out.txt")).expect("artifact");
        assert_eq!(body, "from-env");
        let hashed = sink.events().iter().any(|event| {
            matches!(event, Event::Diagnostic { message } if message.starts_with("artifact out.txt sha256:"))
        });
        assert!(hashed, "artifact digest was not recorded");
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_leaf_is_planned_and_skipped() {
        let document = parse(
            r#"
schema_version = 1

[tasks.win]
platform = "windows"
executor = { native = ["/bin/echo", "ran-windows"] }
"#,
        );
        let mut sink = RecordingSink::new();
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("win".to_owned()),
            },
            FailurePolicy::FailFast,
            Some(&mut sink),
        )
        .expect("plan");
        assert_eq!(plan.nodes.len(), 1);
        let ctx = ExecContext::isolated("");
        let mut run_sink = RecordingSink::new();
        let run = run_operation_plan(&plan, &ctx, 1, Some(&mut run_sink)).expect("run");
        assert_eq!(run.exit_code, 0);
        assert!(run.order.is_empty());
        assert!(run_sink.events().iter().any(|event| {
            matches!(
                event,
                Event::Diagnostic { message } if message.contains("platform `windows`")
            )
        }));
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_remote_leaf_runs_on_the_selected_worker() {
        let document = parse(
            r#"
schema_version = 1

[tasks.win]
platform = "windows"
executor = { remote = { target = "windows-pc", task = "fixture", source_url = "https://example.invalid/repo.git", source_revision = "0123456789abcdef0123456789abcdef01234567", lock_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("win".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert!(matches!(
            plan.nodes[0].operation,
            Some(Operation::Remote { ref target, .. }) if target == "windows-pc"
        ));
        let ctx = ExecContext::isolated("");
        let mut sink = RecordingSink::new();
        let transport = RecordingTransport {
            submits: Mutex::new(Vec::new()),
            fail_submit: false,
        };
        let mut native_build = None;
        let run = run_operation_plan_callbacks(
            &plan,
            &ctx,
            1,
            Some(&mut sink),
            None,
            Some(&mut |operation, _ctx, node| {
                match remote_leaf_path(node) {
                    RemoteLeafPath::LinuxAttempt => {
                        let source = SourceSetV1::fixture("unused");
                        place_linux_leaf(
                            node,
                            Path::new("."),
                            &source,
                            "wf_unused",
                            &transport,
                            |_, _, _, _| Err("windows leaf must not fetch".to_owned()),
                        )
                        .expect("windows leaf must not submit");
                    }
                    RemoteLeafPath::NativeBuild => match operation {
                        Operation::Remote {
                            target,
                            task,
                            source_url,
                            source_revision,
                            lock_digest,
                            build_root,
                            manifest,
                            ..
                        } => {
                            native_build = Some((
                                target.clone(),
                                task.clone(),
                                source_url.clone(),
                                source_revision.clone(),
                                lock_digest.clone(),
                                build_root.clone(),
                                manifest.clone(),
                            ));
                        }
                        other => panic!("native-build callback received {other:?}"),
                    },
                }
                Ok(OperationResult::Exited {
                    code: 0,
                    stdout:
                        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
                            .to_owned(),
                    stderr: String::new(),
                })
            }),
        )
        .expect("run");
        assert!(transport.submits.lock().expect("submits").is_empty());
        assert_windows_native_build(native_build);
        assert_eq!(run.exit_code, 0);
        assert_eq!(run.order, vec!["win".to_owned()]);
        assert!(!sink.events().iter().any(|event| {
            matches!(event, Event::Diagnostic { message } if message.contains("does not match host"))
        }));
    }

    #[cfg(unix)]
    #[test]
    fn linux_leaf_is_the_only_submitted_node() {
        let ran = run_four_node(false);
        let run = ran.result.expect("run");
        assert_eq!(run.exit_code, 0);
        assert_eq!(
            run.order,
            vec![
                "prep-a".to_owned(),
                "prep-b".to_owned(),
                "linux-test".to_owned(),
                "verify".to_owned()
            ]
        );
        assert_eq!(ran.submits.len(), 1);
        assert_linux_submit_body(&ran.submits[0]);
        assert_eq!(
            ran.fetches,
            vec![("att_linux_1".to_owned(), "linux-result.txt".to_owned())]
        );
        assert_eq!(
            std::fs::read_to_string(ran.root.path().join("preparation/alpha.txt")).expect("alpha"),
            "alpha-prep-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(ran.root.path().join("preparation/beta.txt")).expect("beta"),
            "beta-prep-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(ran.root.path().join("linux-result.txt")).expect("artifact"),
            "returned-linux-artifact"
        );
        assert_eq!(
            std::fs::read_to_string(ran.root.path().join("verification/consumer.txt"))
                .expect("consumer"),
            "returned-linux-artifact"
        );
        assert_eq!(
            run.order
                .iter()
                .filter(|id| id.as_str() == "verify")
                .count(),
            1
        );

        let failed = run_four_node(true);
        let error = failed.result.expect_err("failed submit");
        assert!(error.to_string().contains("submit refused"), "{error}");
        assert_eq!(failed.submits.len(), 1);
        assert_linux_submit_body(&failed.submits[0]);
        assert!(failed.fetches.is_empty());
        assert!(!failed.root.path().join("linux-result.txt").exists());
        assert!(!failed.root.path().join("verification").exists());
        assert!(
            !failed
                .root
                .path()
                .join("verification/consumer.txt")
                .exists()
        );
        assert_eq!(
            std::fs::read_to_string(failed.root.path().join("preparation/alpha.txt"))
                .expect("alpha still ran"),
            "alpha-prep-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(failed.root.path().join("preparation/beta.txt"))
                .expect("beta still ran"),
            "beta-prep-bytes"
        );
    }

    #[test]
    fn build_root_must_stay_inside_the_checkout() {
        let escaped = r#"
schema_version = 1

[tasks.win]
executor = { remote = { target = "windows-pc", task = "fixture", source_url = "https://example.invalid/repo.git", source_revision = "0123456789abcdef0123456789abcdef01234567", lock_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", build_root = "../outside" } }
"#;
        let error = parse_workflow_str(escaped, "workflows.toml").expect_err("escape");
        assert!(error.to_string().contains("build_root"));

        let document = parse(
            r#"
schema_version = 1

[tasks.win]
executor = { remote = { target = "windows-pc", task = "build-sidecar", source_url = "https://example.invalid/ecosystem.git", source_revision = "0123456789abcdef0123456789abcdef01234567", lock_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", build_root = "lattice", manifest = "lattice/.winbuild.json" } }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("win".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        match &plan.nodes[0].operation {
            Some(Operation::Remote {
                build_root,
                manifest,
                ..
            }) => {
                assert_eq!(build_root, "lattice");
                assert_eq!(manifest, "lattice/.winbuild.json");
            }
            other => panic!("expected remote operation, got {other:?}"),
        }
    }

    #[test]
    fn development_plan_resolves_one_commit_and_lock_digest() {
        let root = tempfile::tempdir().expect("repo");
        let repo = root.path();
        git(repo, &["init"]);
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README"), "pin\n").unwrap();
        git(repo, &["add", "README"]);
        git(repo, &["commit", "-m", "pin"]);
        let digest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        std::fs::write(
            repo.join("windows.lock.json"),
            format!(r#"{{"digest":"{digest}"}}"#),
        )
        .unwrap();
        std::fs::create_dir(repo.join(".nxr")).unwrap();
        let workflow = r#"
schema_version = 1

[tasks.win]
executor = { remote = { target = "windows-pc", task = "fixture", source_url = "https://example.invalid/repo.git", source_ref = "HEAD", lock_file = "windows.lock.json", build_root = "lattice", manifest = "lattice/.winbuild.json" } }
"#;
        std::fs::write(repo.join(".nxr/workflows.toml"), workflow).unwrap();
        let document = load_workflow_document(repo)
            .expect("load")
            .expect("document");
        let mut plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("win".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let manifests = resolve_plan_pins(repo, &mut plan).expect("resolve");
        let head = git_commit(repo, "HEAD").expect("head");
        assert_eq!(manifests[0]["source_revision"], head);
        assert_eq!(manifests[0]["lock_digest"], digest);
        assert_eq!(manifests[0]["build_root"], "lattice");
        let saved = std::fs::read_to_string(repo.join(".nxr/workflows.toml")).unwrap();
        assert_eq!(saved, workflow);
        match &plan.nodes[0].operation {
            Some(Operation::Remote {
                source_revision,
                source_ref,
                lock_digest,
                ..
            }) => {
                assert_eq!(source_revision, &head);
                assert!(source_ref.is_empty());
                assert_eq!(lock_digest, digest);
            }
            other => panic!("expected resolved remote, got {other:?}"),
        }
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn an_input_names_a_dependency_output() {
        let text = r#"
schema_version = 1

[tasks.producer]
artifacts = ["out.txt"]
executor = { native = ["true"] }

[tasks.consumer]
depends_on = ["producer"]
inputs = ["producer"]
executor = { native = ["true"] }
"#;
        let document = parse_workflow_str(text, "workflows.toml").expect("parse");
        assert_eq!(
            document.tasks["consumer"].inputs,
            vec!["producer".to_owned()]
        );
        let missing = r#"
schema_version = 1

[tasks.consumer]
inputs = ["producer"]
executor = { native = ["true"] }
"#;
        let error = parse_workflow_str(missing, "workflows.toml").expect_err("input");
        assert!(error.to_string().contains("is not a dependency"));
    }

    #[test]
    fn source_member_cross_member_dep_plans_in_order() {
        let document = parse(
            r#"
schema_version = 1

[tasks.typecheck]
source_member = "stargate"
executor = { native = ["/bin/true"] }

[tasks.build]
source_member = "lattice"
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/true"] }

[tasks.remote-win]
executor = { remote = { target = "windows-pc", task = "fixture", source_url = "https://example.invalid/repo.git", source_revision = "0123456789abcdef0123456789abcdef01234567", lock_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
"#,
        );
        assert_eq!(
            document.tasks["build"].depends_on,
            vec!["typecheck".to_owned()]
        );
        assert!(!document.tasks["typecheck"].remote_eligible);
        assert!(!document.tasks["build"].remote_eligible);
        assert!(document.tasks["remote-win"].remote_eligible);

        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("build".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert_eq!(
            plan.serial_order,
            vec!["typecheck".to_owned(), "build".to_owned()]
        );
        let typecheck = plan
            .nodes
            .iter()
            .find(|node| node.id == "typecheck")
            .expect("typecheck");
        assert_eq!(typecheck.source_member.as_deref(), Some("stargate"));
        assert!(!typecheck.remote_eligible);
        let build = plan
            .nodes
            .iter()
            .find(|node| node.id == "build")
            .expect("build");
        assert_eq!(build.source_member.as_deref(), Some("lattice"));
        assert!(!build.remote_eligible);
        assert!(
            plan.nodes.iter().all(|node| node.id != "remote-win"),
            "an unrelated remote task is not part of the build subgraph"
        );
    }

    #[test]
    fn workflows_without_source_member_still_parse_and_plan() {
        let document = parse(
            r#"
schema_version = 1

[tasks.prep]
executor = { native = ["/bin/true"] }
"#,
        );
        assert!(document.tasks["prep"].source_member.is_none());
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("prep".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert!(plan.nodes.iter().all(|node| node.source_member.is_none()));
        assert!(plan.nodes.iter().all(|node| !node.remote_eligible));
    }

    #[test]
    fn unknown_member_dependency_edge_is_invalid() {
        let error = parse_workflow_str(
            r#"
schema_version = 1

[tasks.build]
source_member = "lattice"
depends_on = ["missing:task"]
executor = { native = ["/bin/true"] }
"#,
            "workflows.toml",
        )
        .expect_err("missing member edge");
        assert!(matches!(error, WorkflowError::Invalid { .. }));
        assert!(error.to_string().contains("missing:task"));
    }

    #[cfg(unix)]
    #[test]
    fn source_member_native_graph_runs_in_dependency_order() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("stargate")).expect("stargate");
        std::fs::create_dir(root.path().join("lattice")).expect("lattice");
        let document = parse(
            r#"
schema_version = 1

[tasks.typecheck]
source_member = "stargate"
working_directory = "."
executor = { native = ["/bin/sh", "-c", "echo typecheck >> ../marker"] }

[tasks.build]
source_member = "lattice"
working_directory = "."
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/sh", "-c", "echo build >> ../marker"] }

[tasks.test]
source_member = "lattice"
working_directory = "."
depends_on = ["build"]
executor = { native = ["/bin/sh", "-c", "echo test >> ../marker"] }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("test".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), root.path().join("stargate"));
        member_roots.insert("lattice".to_owned(), root.path().join("lattice"));
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::new(),
            member_roots,
        };
        let run = run_operation_plan(&plan, &ctx, 1, None).expect("run");
        assert_eq!(run.exit_code, 0);
        assert_eq!(
            run.order,
            vec![
                "typecheck".to_owned(),
                "build".to_owned(),
                "test".to_owned()
            ]
        );
        let marker = std::fs::read_to_string(root.path().join("marker")).expect("marker contents");
        assert_eq!(marker, "typecheck\nbuild\ntest\n");
    }

    #[cfg(unix)]
    #[test]
    fn source_member_fail_fast_stops_after_failed_cross_member_leaf() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("stargate")).expect("stargate");
        std::fs::create_dir(root.path().join("lattice")).expect("lattice");
        let document = parse(
            r#"
schema_version = 1

[tasks.typecheck]
source_member = "stargate"
working_directory = "."
executor = { native = ["/bin/sh", "-c", "exit 7"] }

[tasks.build]
source_member = "lattice"
working_directory = "."
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/sh", "-c", "echo build >> ../marker"] }

[tasks.test]
source_member = "lattice"
working_directory = "."
depends_on = ["build"]
executor = { native = ["/bin/sh", "-c", "echo test >> ../marker"] }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("test".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), root.path().join("stargate"));
        member_roots.insert("lattice".to_owned(), root.path().join("lattice"));
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: Some(root.path().to_path_buf()),
            env: BTreeMap::new(),
            member_roots,
        };
        let run = run_operation_plan(&plan, &ctx, 2, None).expect("run");
        assert_eq!(run.exit_code, 7);
        assert_eq!(run.order, vec!["typecheck".to_owned()]);
        assert!(!root.path().join("marker").exists());
    }

    #[test]
    fn invalidated_by_member_collects_member_tasks_and_dependents() {
        let document = parse(
            r#"
schema_version = 1

[tasks.typecheck]
source_member = "stargate"
executor = { native = ["/bin/true"] }

[tasks.build]
source_member = "lattice"
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/true"] }

[tasks.test]
source_member = "lattice"
depends_on = ["build"]
executor = { native = ["/bin/true"] }

[tasks.nxr-only]
source_member = "nxr"
executor = { native = ["/bin/true"] }
"#,
        );
        let stargate = invalidated_by_member(&document, "stargate");
        assert_eq!(
            stargate,
            BTreeSet::from([
                "typecheck".to_owned(),
                "build".to_owned(),
                "test".to_owned(),
            ])
        );
        assert!(!stargate.contains("nxr-only"));
        assert!(invalidated_by_member(&document, "cell").is_empty());
    }

    #[test]
    fn colon_task_key_is_preferred_over_member_split() {
        let document = parse(
            r#"
schema_version = 1

[tasks."lattice:test"]
source_member = "lattice"
executor = { native = ["/bin/true"] }

[tasks."stargate:test"]
source_member = "stargate"
executor = { native = ["/bin/true"] }

[tasks.test]
source_member = "lattice"
executor = { native = ["/bin/true"] }

[tasks.consumer]
depends_on = ["lattice:test"]
executor = { native = ["/bin/true"] }
"#,
        );
        assert_eq!(
            document.tasks["consumer"].depends_on,
            vec!["lattice:test".to_owned()]
        );
        assert!(document.tasks.contains_key("stargate:test"));
        assert!(document.tasks.contains_key("test"));
    }

    #[cfg(unix)]
    #[test]
    fn two_member_checkouts_write_artifacts_and_receipts() {
        let stargate = tempfile::tempdir().expect("stargate");
        let lattice = tempfile::tempdir().expect("lattice");
        let document = parse(
            r#"
schema_version = 1

[tasks."stargate:typecheck"]
source_member = "stargate"
artifacts = ["out.txt"]
executor = { native = ["/bin/sh", "-c", "printf ok > out.txt"] }

[tasks."lattice:test"]
source_member = "lattice"
artifacts = ["out.txt"]
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/sh", "-c", "printf ok > out.txt"] }
"#,
        );
        assert_eq!(
            document.tasks["lattice:test"].depends_on,
            vec!["stargate:typecheck".to_owned()]
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("lattice:test".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), stargate.path().to_path_buf());
        member_roots.insert("lattice".to_owned(), lattice.path().to_path_buf());
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots,
        };
        let run = run_operation_plan(&plan, &ctx, 1, None).expect("run");
        assert_eq!(run.exit_code, 0);
        assert_eq!(
            run.order,
            vec!["stargate:typecheck".to_owned(), "lattice:test".to_owned()]
        );
        assert_eq!(
            std::fs::read_to_string(stargate.path().join("out.txt")).expect("stargate out"),
            "ok"
        );
        assert_eq!(
            std::fs::read_to_string(lattice.path().join("out.txt")).expect("lattice out"),
            "ok"
        );
        let stargate_receipt = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(
                stargate
                    .path()
                    .join(".nxr/receipts/stargate:typecheck.json"),
            )
            .expect("stargate receipt"),
        )
        .expect("parse stargate receipt");
        assert_eq!(stargate_receipt["node"], "stargate:typecheck");
        assert_eq!(stargate_receipt["source_member"], "stargate");
        assert!(stargate_receipt["artifacts"]["out.txt"].is_string());
        let lattice_receipt = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(lattice.path().join(".nxr/receipts/lattice:test.json"))
                .expect("lattice receipt"),
        )
        .expect("parse lattice receipt");
        assert_eq!(lattice_receipt["node"], "lattice:test");
        assert_eq!(lattice_receipt["source_member"], "lattice");
        assert!(lattice_receipt["artifacts"]["out.txt"].is_string());
    }

    #[cfg(unix)]
    #[test]
    fn two_member_fail_fast_skips_dependent_checkout_artifact() {
        let stargate = tempfile::tempdir().expect("stargate");
        let lattice = tempfile::tempdir().expect("lattice");
        let document = parse(
            r#"
schema_version = 1

[tasks."stargate:typecheck"]
source_member = "stargate"
artifacts = ["out.txt"]
executor = { native = ["/bin/sh", "-c", "exit 7"] }

[tasks."lattice:test"]
source_member = "lattice"
artifacts = ["out.txt"]
depends_on = ["stargate:typecheck"]
executor = { native = ["/bin/sh", "-c", "printf ok > out.txt"] }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("lattice:test".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), stargate.path().to_path_buf());
        member_roots.insert("lattice".to_owned(), lattice.path().to_path_buf());
        let ctx = ExecContext {
            path: std::env::var("PATH").unwrap_or_default(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots,
        };
        let run = run_operation_plan(&plan, &ctx, 2, None).expect("run");
        assert_eq!(run.exit_code, 7);
        assert_eq!(run.order, vec!["stargate:typecheck".to_owned()]);
        assert!(!lattice.path().join("out.txt").exists());
        assert!(
            !lattice
                .path()
                .join(".nxr/receipts/lattice:test.json")
                .exists()
        );
    }
}
