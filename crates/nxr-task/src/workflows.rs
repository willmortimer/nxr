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
//! executor = { native = ["/bin/echo", "prep"] }
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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nxr_core::Operation;
use serde::Deserialize;
use thiserror::Error;

use crate::events::{Event, EventSink};
use crate::graph::{GraphError, TaskGraph};
use crate::plan_exec::{ArgumentForwarding, ExecutionPlan, FailurePolicy, PlanNode};
use crate::planner::plan_serial_union;
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
    pub operation: Operation,
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
        let resources = task
            .resources
            .as_ref()
            .map(NodeResources::from_task_resources)
            .unwrap_or_default();
        tasks.insert(
            name,
            WorkflowTask {
                depends_on: task.depends_on,
                platform,
                architecture: task.architecture,
                resources,
                environment: task.env,
                working_directory: task.working_directory,
                artifacts: task.artifacts,
                operation,
            },
        );
    }

    Ok(WorkflowDocument {
        schema_version: file.schema_version,
        tasks,
    })
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nxr_core::Operation;
    use nxr_core::diagnostics::exit;

    use super::*;
    use crate::events::{RecordingSink, event_kind};
    use crate::execute::{ExecContext, run_operation_plan, run_operation_plan_callbacks};

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
            let ctx = ExecContext {
                path: trap_dir.path().display().to_string(),
                nix_override: Some("/definitely/missing/nix".to_owned()),
                cwd: None,
                env: BTreeMap::new(),
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

    #[cfg(unix)]
    fn write_trap(dir: &std::path::Path, name: &str, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        std::fs::write(&program, script).expect("write trap");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod trap");
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
        let run = run_operation_plan_callbacks(
            &plan,
            &ctx,
            1,
            Some(&mut sink),
            None,
            Some(&mut |_operation, _ctx| {
                Ok(crate::execute::OperationResult::Exited {
                    code: 0,
                    stdout:
                        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
                            .to_owned(),
                    stderr: String::new(),
                })
            }),
        )
        .expect("run");
        assert_eq!(run.exit_code, 0);
        assert_eq!(run.order, vec!["win".to_owned()]);
        assert!(!sink.events().iter().any(|event| {
            matches!(event, Event::Diagnostic { message } if message.contains("does not match host"))
        }));
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
}
