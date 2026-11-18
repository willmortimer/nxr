//! Dispatch a plan node's [`nxr_core::Operation`].
//!
//! The scheduler stays graph-pure. This module is the only place that runs an
//! operation. Native commands and winbuild tasks do not locate Nix and do not
//! start WSL. Nix-backed operations are not spawned here: a missing Nix
//! executable is [`ExecuteError::UnavailableExecutor`], and a present Nix
//! executable stays on the existing `nix run` path.
//!
//! Windows Job Objects are not implemented. Native argv uses
//! [`std::process::Command`] directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nxr_core::Operation;
use nxr_core::diagnostics::exit;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::events::{Event, EventSink, NodeOutcome, OutputPayload};
use crate::plan_exec::{ExecutionPlan, PlanNode};
use crate::scheduler::{Scheduler, SchedulerError};
use crate::workflows::host_skip_reason;

/// Machine-readable code for a missing executor. Not a new process exit status.
pub const UNAVAILABLE_EXECUTOR: &str = "unavailable-executor";

/// Which executor a node needed and could not find.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutorName {
    /// `nix` for flake apps, nix builds, and workspace scripts.
    Nix,
    /// `winbuild` for [`Operation::WinBuildTask`].
    Winbuild,
    /// Remote execution, which phase 1.1 does not dial.
    Remote,
    /// WASI component tier (V4.5 scaffolding).
    Wasi,
    /// External adapter hook (Hub inspector, deploy guard, …).
    Adapter,
}

impl ExecutorName {
    /// Stable label used in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nix => "nix",
            Self::Winbuild => "winbuild",
            Self::Remote => "remote",
            Self::Wasi => "wasi",
            Self::Adapter => "adapter",
        }
    }

    /// Existing public exit code for this missing executor.
    ///
    /// Nix maps to [`exit::NIX_CAPABILITY`] (the same status as `NixNotFound`).
    /// Winbuild and remote map to [`exit::PROCESS_SUPERVISION`].
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Nix => exit::NIX_CAPABILITY,
            Self::Winbuild | Self::Remote | Self::Wasi | Self::Adapter => exit::PROCESS_SUPERVISION,
        }
    }
}

impl std::fmt::Display for ExecutorName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Environment the operation runner is allowed to consult.
///
/// Native commands and winbuild tasks ignore [`Self::nix_override`]. Nix
/// lookup uses only this struct: it does not read `NXR_NIX` or the process
/// `PATH` unless the caller copied them in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecContext {
    /// `PATH` used for relative native programs and for `winbuild` / `nix` probes.
    pub path: String,
    /// Explicit `nix` binary. `None` searches [`Self::path`].
    pub nix_override: Option<String>,
    /// Working directory for native and winbuild children.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables. `PATH` still comes from [`Self::path`].
    pub env: BTreeMap<String, String>,
    /// Checkout roots keyed by workflow `source_member`. Required for named members.
    pub member_roots: BTreeMap<String, PathBuf>,
}

impl ExecContext {
    /// Context that sees only `path` and does not override Nix.
    #[must_use]
    pub fn isolated(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        }
    }
}

/// Result of dispatching one operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationResult {
    /// Child ran. `code` is the process status (`1` when the status is missing).
    Exited {
        code: i32,
        stdout: String,
        stderr: String,
    },
    /// A Nix binary is present. This runner does not invoke it.
    NixPresent { program: String },
}

/// Failure while dispatching an operation.
#[derive(Debug, Error)]
pub enum ExecuteError {
    /// The executor this node needs is not available.
    #[error("unavailable executor `{executor}`: {detail}")]
    UnavailableExecutor {
        executor: ExecutorName,
        detail: String,
    },
    /// Nix was found, and realization stays on `nix run` / `nix build`.
    #[error("nix executor at `{program}` is present; this runner does not invoke nix")]
    NixNotInvoked { program: String },
    /// A native or winbuild child could not be spawned.
    #[error("failed to spawn operation: {detail}")]
    Spawn { detail: String },
    /// The operation payload itself cannot be run.
    #[error("invalid operation: {detail}")]
    Invalid { detail: String },
    /// A plan node has no desugared operation.
    #[error("plan node `{node}` has no operation")]
    MissingOperation { node: String },
    /// The graph scheduler rejected the plan or a completion.
    #[error("operation scheduler failed: {detail}")]
    Scheduler { detail: String },
    /// A workflow `source_member` could not be resolved to an isolated checkout.
    #[error("source member `{member}` is unresolved: {detail}")]
    SourceUnresolved { member: String, detail: String },
}

impl ExecuteError {
    /// Stable diagnostic code. [`Self::UnavailableExecutor`] is `unavailable-executor`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnavailableExecutor { .. } => UNAVAILABLE_EXECUTOR,
            Self::NixNotInvoked { .. } => "nix-not-invoked",
            Self::Spawn { .. } => "spawn-failed",
            Self::Invalid { .. } => "invalid-operation",
            Self::MissingOperation { .. } => "missing-operation",
            Self::Scheduler { .. } => "scheduler",
            Self::SourceUnresolved { .. } => "source-unresolved",
        }
    }

    /// Map onto the existing public exit-code range. No new status is introduced.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::UnavailableExecutor { executor, .. } => executor.exit_code(),
            Self::NixNotInvoked { .. } | Self::Spawn { .. } | Self::Scheduler { .. } => {
                exit::PROCESS_SUPERVISION
            }
            Self::Invalid { .. } => exit::USAGE,
            Self::MissingOperation { .. } | Self::SourceUnresolved { .. } => exit::TASK_GRAPH,
        }
    }

    fn unavailable(executor: ExecutorName, detail: impl Into<String>) -> Self {
        Self::UnavailableExecutor {
            executor,
            detail: detail.into(),
        }
    }
}

impl From<SchedulerError> for ExecuteError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler {
            detail: error.to_string(),
        }
    }
}

/// Completed native (or mixed) run driven by the existing scheduler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRun {
    /// `0` when every started node exited 0, otherwise the first failing status.
    pub exit_code: i32,
    /// Node ids in the order they ran.
    pub order: Vec<String>,
    /// UTF-8 stdout of successful and failed children, in run order.
    pub stdout: String,
}

/// Run `operation` under `ctx`.
///
/// # Errors
///
/// Returns [`ExecuteError::UnavailableExecutor`] when a Nix-backed operation
/// has no Nix executable, when `winbuild` is absent, or when a remote target
/// cannot be dialed. Native spawn failures are [`ExecuteError::Spawn`].
pub fn execute_operation(
    operation: &Operation,
    ctx: &ExecContext,
) -> Result<OperationResult, ExecuteError> {
    match operation {
        Operation::NativeCommand { argv } => execute_native(argv, ctx),
        Operation::WinBuildTask { task_id, manifest } => execute_winbuild(task_id, manifest, ctx),
        Operation::FlakeApp { .. }
        | Operation::NixBuild { .. }
        | Operation::WorkspaceScript { .. }
        | Operation::Process { .. } => execute_nix_backed(ctx),
        Operation::WasiComponent { component, .. } => Err(ExecuteError::unavailable(
            ExecutorName::Wasi,
            format!("WASI component `{component}` is not available"),
        )),
        Operation::Adapter { adapter_kind, .. } => Err(ExecuteError::unavailable(
            ExecutorName::Adapter,
            format!("adapter `{adapter_kind}` is not available"),
        )),
        Operation::Remote { target, .. } => Err(ExecuteError::unavailable(
            ExecutorName::Remote,
            format!("remote executor `{target}` is not available"),
        )),
    }
}

/// Drive `plan` with the scheduler and dispatch each started node's operation.
///
/// `jobs` is the scheduler slot cap. This function does not interpret waves.
///
/// # Errors
///
/// Returns [`ExecuteError`] when a node cannot be dispatched. A child that
/// exits nonzero is recorded on [`OperationRun::exit_code`] and still follows
/// the plan's failure policy inside the scheduler.
pub fn run_operation_plan(
    plan: &ExecutionPlan,
    ctx: &ExecContext,
    jobs: usize,
    sink: Option<&mut dyn EventSink>,
) -> Result<OperationRun, ExecuteError> {
    run_operation_plan_with(plan, ctx, jobs, sink, None)
}

/// Same as [`run_operation_plan`], but flake, nix-build, and workspace-script
/// nodes are handed to `nix` instead of stopping at [`ExecuteError::NixNotInvoked`].
///
/// `nix` receives the operation after a Nix executable has already been found.
///
/// # Errors
///
/// Returns [`ExecuteError`] from scheduling, native spawn, or `nix`.
pub fn run_operation_plan_with(
    plan: &ExecutionPlan,
    ctx: &ExecContext,
    jobs: usize,
    sink: Option<&mut dyn EventSink>,
    nix: Option<&mut dyn FnMut(&Operation, &ExecContext) -> Result<OperationResult, ExecuteError>>,
) -> Result<OperationRun, ExecuteError> {
    run_operation_plan_callbacks(plan, ctx, jobs, sink, nix, None)
}

/// Same as [`run_operation_plan_with`], and remote leaves go to `remote`.
///
/// `remote` receives the operation, the node context, and the plan node being
/// placed. Local nodes are not passed to it.
///
/// # Errors
///
/// Returns [`ExecuteError`] from scheduling, native spawn, `nix`, or `remote`.
pub fn run_operation_plan_callbacks(
    plan: &ExecutionPlan,
    ctx: &ExecContext,
    jobs: usize,
    mut sink: Option<&mut dyn EventSink>,
    mut nix: Option<
        &mut dyn FnMut(&Operation, &ExecContext) -> Result<OperationResult, ExecuteError>,
    >,
    mut remote: Option<
        &mut dyn FnMut(
            &Operation,
            &ExecContext,
            &PlanNode,
        ) -> Result<OperationResult, ExecuteError>,
    >,
) -> Result<OperationRun, ExecuteError> {
    let mut scheduler = Scheduler::new(plan, jobs)?;
    let mut order = Vec::new();
    let mut stdout = String::new();
    let mut first_failure: Option<i32> = None;

    loop {
        if scheduler.is_finished() {
            break;
        }
        let started = scheduler.schedule_ready();
        if started.is_empty() {
            break;
        }
        for id in started {
            let node = plan
                .nodes
                .iter()
                .find(|node| node.id == id)
                .ok_or_else(|| ExecuteError::Scheduler {
                    detail: format!("scheduler started unknown node `{id}`"),
                })?;
            if let Some(reason) = host_skip_reason(
                node.platform.as_deref(),
                node.architecture.as_deref(),
                matches!(node.operation, Some(Operation::Remote { .. })),
            ) {
                emit(
                    sink.as_mut(),
                    Event::Diagnostic {
                        message: format!("skipped `{id}`: {reason}"),
                    },
                );
                scheduler.on_exit(&id, exit::SUCCESS)?;
                emit(
                    sink.as_mut(),
                    Event::node_finished(
                        id,
                        Some(exit::SUCCESS),
                        NodeOutcome::Skipped,
                        Some(reason),
                    ),
                );
                continue;
            }
            let Some(operation) = node.operation.as_ref() else {
                return Err(ExecuteError::MissingOperation { node: id });
            };
            let node_ctx = context_for_node(ctx, node)?;
            if let Err(detail) = crate::artifact::materialize_inputs(&node_ctx, &node.inputs) {
                return Err(ExecuteError::Invalid { detail });
            }
            emit(sink.as_mut(), Event::node_queued(id.clone()));
            emit(sink.as_mut(), Event::node_started(id.clone()));
            let executed = if matches!(operation, Operation::Remote { .. }) {
                match remote.as_mut() {
                    Some(dispatch) => dispatch(operation, &node_ctx, node),
                    None => Err(ExecuteError::unavailable(
                        ExecutorName::Remote,
                        "remote executor is not available".to_owned(),
                    )),
                }
            } else {
                match execute_operation(operation, &node_ctx) {
                    Ok(OperationResult::NixPresent { program }) => match nix.as_mut() {
                        Some(dispatch) => dispatch(operation, &node_ctx),
                        None => Err(ExecuteError::NixNotInvoked { program }),
                    },
                    other => other,
                }
            };
            match executed {
                Ok(OperationResult::Exited {
                    mut code,
                    stdout: node_stdout,
                    stderr,
                }) => {
                    if !node_stdout.is_empty() {
                        emit(
                            sink.as_mut(),
                            Event::StdoutChunk {
                                node: id.clone(),
                                payload: OutputPayload::from_bytes(
                                    node_stdout.clone().into_bytes(),
                                ),
                            },
                        );
                        stdout.push_str(&node_stdout);
                    }
                    if !stderr.is_empty() {
                        emit(
                            sink.as_mut(),
                            Event::StderrChunk {
                                node: id.clone(),
                                payload: OutputPayload::from_bytes(stderr.into_bytes()),
                            },
                        );
                    }
                    if code == 0
                        && let Err(detail) = record_artifacts(node, &node_ctx, &id, sink.as_mut())
                    {
                        code = exit::CHILD_FAILED;
                        emit(sink.as_mut(), Event::Diagnostic { message: detail });
                    }
                    if code != 0 && first_failure.is_none() {
                        first_failure = Some(code);
                    }
                    order.push(id.clone());
                    scheduler.on_exit(&id, code)?;
                    let status = if code == 0 {
                        NodeOutcome::Succeeded
                    } else {
                        NodeOutcome::Failed
                    };
                    emit(
                        sink.as_mut(),
                        Event::node_finished(id, Some(code), status, None),
                    );
                }
                Ok(OperationResult::NixPresent { program }) => {
                    emit(
                        sink.as_mut(),
                        Event::node_finished(id.clone(), None, NodeOutcome::Failed, None),
                    );
                    return Err(ExecuteError::NixNotInvoked { program });
                }
                Err(error) => {
                    emit(
                        sink.as_mut(),
                        Event::node_finished(
                            id,
                            None,
                            NodeOutcome::Failed,
                            Some(error.to_string()),
                        ),
                    );
                    return Err(error);
                }
            }
        }
    }

    let success = scheduler.outcome().success && first_failure.is_none();
    emit(sink.as_mut(), Event::run_completed(success));
    Ok(OperationRun {
        exit_code: first_failure.unwrap_or(exit::SUCCESS),
        order,
        stdout,
    })
}

const ARTIFACT_ROOT_ESCAPE: &str = "artifact root escapes the member checkout";

fn source_unresolved(member: impl Into<String>, detail: impl Into<String>) -> ExecuteError {
    ExecuteError::SourceUnresolved {
        member: member.into(),
        detail: detail.into(),
    }
}

fn member_name_addressable(member: &str) -> bool {
    !member.is_empty()
        && member
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn path_entry_is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .is_some_and(|meta| meta.file_type().is_symlink())
}

fn walk_member_working_directory(
    member: &str,
    root: &Path,
    relative: &str,
) -> Result<PathBuf, ExecuteError> {
    use std::path::Component;
    let relative_path = Path::new(relative);
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => {
                current.push(part);
                if path_entry_is_symlink(&current) {
                    return Err(source_unresolved(
                        member,
                        "working directory escapes through a symlink",
                    ));
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(source_unresolved(
                    member,
                    "working directory escapes through a symlink",
                ));
            }
        }
    }
    Ok(current)
}

fn expected_revision_key(member: &str) -> String {
    format!("NXR_EXPECTED_REVISION_{member}")
}

fn observed_revision_key(member: &str) -> String {
    format!("NXR_OBSERVED_REVISION_{member}")
}

fn verify_source_revision(
    member: &str,
    env: &BTreeMap<String, String>,
) -> Result<(), ExecuteError> {
    let expected_key = expected_revision_key(member);
    let Some(expected) = env.get(&expected_key) else {
        return Ok(());
    };
    let observed_key = observed_revision_key(member);
    match env.get(&observed_key) {
        Some(observed) if observed == expected => Ok(()),
        _ => Err(source_unresolved(member, "source revision does not match")),
    }
}

fn context_for_node(base: &ExecContext, node: &PlanNode) -> Result<ExecContext, ExecuteError> {
    let mut env = base.env.clone();
    env.extend(node.environment.clone());
    let member_root = match node.source_member.as_deref() {
        Some(member) => {
            if !member_name_addressable(member) {
                return Err(source_unresolved(
                    member,
                    "source member name is not addressable",
                ));
            }
            let Some(root) = base.member_roots.get(member).cloned() else {
                return Err(source_unresolved(member, "source member is not mounted"));
            };
            if path_entry_is_symlink(&root) {
                return Err(source_unresolved(member, "member root is a symlink"));
            }
            verify_source_revision(member, &base.env)?;
            Some((member.to_owned(), root))
        }
        None => base.cwd.clone().map(|root| (String::new(), root)),
    };
    let cwd = match (&node.working_directory, &member_root) {
        (Some(relative), Some((member, root))) if !member.is_empty() => {
            Some(walk_member_working_directory(member, root, relative)?)
        }
        (Some(relative), Some((_, root))) => Some(root.join(relative)),
        (Some(relative), None) => Some(PathBuf::from(relative)),
        (None, Some((_, root))) => Some(root.clone()),
        (None, None) => None,
    };
    if !node.inputs.is_empty() {
        env.insert("NXR_ARTIFACT_INPUTS".to_owned(), node.inputs.join(","));
        if let Some(root) = &cwd {
            env.insert(
                "NXR_ARTIFACT_MATERIALIZED".to_owned(),
                root.join(".nxr")
                    .join("artifacts")
                    .join("materialized")
                    .join(&node.inputs[0])
                    .display()
                    .to_string(),
            );
        }
    }
    Ok(ExecContext {
        path: base.path.clone(),
        nix_override: base.nix_override.clone(),
        cwd,
        env,
        member_roots: base.member_roots.clone(),
    })
}

fn resolve_member_artifact(cwd: &Path, artifact: &str) -> Result<PathBuf, String> {
    if artifact.is_empty() || Path::new(artifact).is_absolute() {
        return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
    }
    if artifact.split(['/', '\\']).any(|part| part == "..") {
        return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
    }
    let full = cwd.join(artifact);
    if full.strip_prefix(cwd).is_err() {
        return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
    }
    let mut cursor = full.as_path();
    loop {
        if path_entry_is_symlink(cursor) {
            return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
        }
        if cursor == cwd {
            break;
        }
        cursor = cursor.parent().unwrap_or(cwd);
    }
    Ok(full)
}

fn record_artifacts(
    node: &PlanNode,
    ctx: &ExecContext,
    id: &str,
    mut sink: Option<&mut &mut dyn EventSink>,
) -> Result<(), String> {
    let mut artifact_digests = BTreeMap::new();
    for artifact in &node.artifacts {
        let path = match &ctx.cwd {
            Some(cwd) => {
                let published = cwd.join(".nxr/current").join(artifact);
                if published.is_file() {
                    published
                } else {
                    resolve_member_artifact(cwd, artifact)?
                }
            }
            None => {
                let relative = PathBuf::from(artifact);
                if artifact.split(['/', '\\']).any(|part| part == "..") {
                    return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
                }
                if path_entry_is_symlink(&relative) {
                    return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
                }
                relative
            }
        };
        let bytes = std::fs::read(&path).map_err(|_| {
            format!(
                "artifact `{artifact}` for `{id}` is missing at {}",
                path.display()
            )
        })?;
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if let Some(inner) = &mut sink {
            inner.emit(Event::Diagnostic {
                message: format!("artifact {artifact} sha256:{hex}"),
            });
        }
        artifact_digests.insert(artifact.clone(), hex);
    }
    if let Some(cwd) = &ctx.cwd {
        crate::artifact::publish_checkout_catalog(cwd)?;
    }
    if let Some(member) = node.source_member.as_deref()
        && !artifact_digests.is_empty()
        && let Some(checkout) = ctx.member_roots.get(member)
    {
        write_member_receipt(checkout, id, member, &artifact_digests)?;
    }
    Ok(())
}

fn write_member_receipt(
    checkout: &Path,
    node: &str,
    source_member: &str,
    artifacts: &BTreeMap<String, String>,
) -> Result<(), String> {
    if path_entry_is_symlink(checkout) {
        return Err(ARTIFACT_ROOT_ESCAPE.to_owned());
    }
    let receipts = checkout.join(".nxr").join("receipts");
    std::fs::create_dir_all(&receipts).map_err(|error| format!("create receipts dir: {error}"))?;
    let path = receipts.join(format!("{node}.json"));
    let body = serde_json::json!({
        "node": node,
        "source_member": source_member,
        "artifacts": artifacts,
    });
    std::fs::write(&path, format!("{body}\n"))
        .map_err(|error| format!("write receipt {}: {error}", path.display()))
}

fn emit(sink: Option<&mut &mut dyn EventSink>, event: Event) {
    if let Some(sink) = sink {
        sink.emit(event);
    }
}

fn execute_native(argv: &[String], ctx: &ExecContext) -> Result<OperationResult, ExecuteError> {
    let Some(program) = argv.first() else {
        return Err(ExecuteError::Invalid {
            detail: "native command argv is empty".to_owned(),
        });
    };
    let resolved = resolve_native_program(program, &ctx.path)?;
    spawn_argv(&resolved, &argv[1..], ctx)
}

fn execute_winbuild(
    task_id: &str,
    manifest: &str,
    ctx: &ExecContext,
) -> Result<OperationResult, ExecuteError> {
    // Search only for winbuild. Do not look up or spawn wsl.
    let Some(program) = find_program(&ctx.path, "winbuild") else {
        return Err(ExecuteError::unavailable(
            ExecutorName::Winbuild,
            "winbuild executable not found on PATH",
        ));
    };
    let manifest_path = resolve_manifest(ctx.cwd.as_deref(), manifest)?;
    let result = spawn_argv(
        &program,
        &[
            "run".to_owned(),
            task_id.to_owned(),
            "--file".to_owned(),
            manifest_path,
            "--json".to_owned(),
        ],
        ctx,
    )?;
    Ok(normalize_winbuild_output(result))
}

/// Resolve a workspace-relative WinBuild manifest. `.` and an empty value mean
/// `.winbuild.json` beside the selected workspace.
fn resolve_manifest(cwd: Option<&Path>, manifest: &str) -> Result<String, ExecuteError> {
    let relative = if manifest.is_empty() {
        ".winbuild.json"
    } else {
        manifest
    };
    if Path::new(relative).is_absolute() || relative.split(['/', '\\']).any(|part| part == "..") {
        return Err(ExecuteError::Invalid {
            detail: format!("winbuild manifest `{relative}` escapes the workspace"),
        });
    }
    let base = cwd.unwrap_or_else(|| Path::new("."));
    Ok(base.join(relative).display().to_string())
}

/// Split WinBuild `--json` NDJSON into stdout and stderr text.
///
/// `started` and `finished` stay on stdout so the terminal event is recorded.
/// `output` follows its `stream`. Lines that are not JSON stay on stdout.
fn normalize_winbuild_output(result: OperationResult) -> OperationResult {
    let OperationResult::Exited {
        code,
        stdout,
        stderr,
    } = result
    else {
        return result;
    };
    let mut out = String::new();
    let mut err = stderr;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        match value.get("event").and_then(|item| item.as_str()) {
            Some("output") => {
                let text = value
                    .get("text")
                    .and_then(|item| item.as_str())
                    .unwrap_or("");
                let stream = value.get("stream").and_then(|item| item.as_str());
                if stream == Some("stderr") {
                    err.push_str(text);
                    if !text.ends_with('\n') {
                        err.push('\n');
                    }
                } else {
                    out.push_str(text);
                    if !text.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            Some("started" | "finished" | "progress") => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    OperationResult::Exited {
        code,
        stdout: out,
        stderr: err,
    }
}

fn execute_nix_backed(ctx: &ExecContext) -> Result<OperationResult, ExecuteError> {
    let program = require_nix(ctx)?;
    Ok(OperationResult::NixPresent {
        program: program.display().to_string(),
    })
}

/// Nix probe used only by flake, nix-build, and workspace-script operations.
fn require_nix(ctx: &ExecContext) -> Result<PathBuf, ExecuteError> {
    if let Some(explicit) = &ctx.nix_override {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(ExecuteError::unavailable(
            ExecutorName::Nix,
            format!("nix executable not found at {explicit}"),
        ));
    }
    find_program(&ctx.path, "nix").ok_or_else(|| {
        ExecuteError::unavailable(ExecutorName::Nix, "nix executable not found on PATH")
    })
}

fn resolve_native_program(argv0: &str, path: &str) -> Result<PathBuf, ExecuteError> {
    if argv0.contains('/') || argv0.contains('\\') {
        let program = PathBuf::from(argv0);
        if program.is_file() {
            return Ok(program);
        }
        return Err(ExecuteError::Spawn {
            detail: format!("native program `{argv0}` not found"),
        });
    }
    find_program(path, argv0).ok_or_else(|| ExecuteError::Spawn {
        detail: format!("native program `{argv0}` not found on PATH"),
    })
}

fn find_program(path: &str, name: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn spawn_argv(
    program: &Path,
    args: &[String],
    ctx: &ExecContext,
) -> Result<OperationResult, ExecuteError> {
    let mut argv = vec![program.display().to_string()];
    argv.extend(args.iter().cloned());
    let root = ctx.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    let coverage = nxr_core::output_resource::classify_output_coverage(
        &root,
        &argv,
        std::env::var("CARGO_TARGET_DIR").ok().as_deref(),
        None,
        std::env::consts::OS,
        std::env::consts::ARCH,
        "local",
    );
    let output_dirs = match coverage {
        nxr_core::output_resource::OutputCoverage::Managed(resources) => {
            let reserve = nxr_core::disk_preflight::disk_reserve_bytes(
                std::env::var("NXR_DISK_RESERVE_BYTES").ok().as_deref(),
                nxr_core::admission_budget::host_budget(None, None, 1).memory_mib,
            );
            for resource in &resources {
                let decision = nxr_core::disk_preflight::disk_preflight(&resource.path, reserve);
                if !decision.allowed {
                    return Err(ExecuteError::Spawn {
                        detail: decision.reason,
                    });
                }
            }
            resources
                .into_iter()
                .map(|resource| resource.path)
                .collect()
        }
        nxr_core::output_resource::OutputCoverage::NoneRequired
        | nxr_core::output_resource::OutputCoverage::Unknown { .. } => Vec::new(),
    };
    let mut env = vec![("PATH".to_owned(), ctx.path.clone())];
    env.extend(
        ctx.env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    for (key, value) in crate::compiler_cache::compiler_cache_assignments(
        &crate::compiler_cache::compiler_cache_from_process(),
    ) {
        if !ctx.env.contains_key(&key) {
            env.push((key, value));
        }
    }
    let request = nxr_core::resource_guard::WriterRequest {
        argv,
        cwd: ctx.cwd.clone(),
        env,
        output_dirs,
        stdio: nxr_core::resource_guard::WriterStdio::Piped,
        probe_out: None,
        probe_script: None,
        guardian_bin: None,
        pause_file: None,
        clear_env: true,
    };
    let launched = nxr_core::resource_guard::launch_writer(&request)
        .map_err(|detail| ExecuteError::Spawn { detail })?;
    let writer_pid = launched.writer_pid;
    let admissions = launched.admissions;
    let child = launched
        .child
        .or(launched.guardian)
        .ok_or_else(|| ExecuteError::Spawn {
            detail: "writer was not started".to_owned(),
        })?;
    let output = child
        .wait_with_output()
        .map_err(|error| ExecuteError::Spawn {
            detail: format!("failed to wait {}: {error}", program.display()),
        })?;
    if !admissions.is_empty() {
        release_leaf_when_stopped(writer_pid, admissions);
    }
    let code = output.status.code().unwrap_or(exit::CHILD_FAILED);
    Ok(OperationResult::Exited {
        code,
        stdout: decode_output(&output.stdout),
        stderr: decode_output(&output.stderr),
    })
}

fn release_leaf_when_stopped(pid: u32, admissions: Vec<nxr_core::target_lock::TargetAdmission>) {
    std::thread::spawn(move || {
        loop {
            match nxr_core::resource_guard::writer_group_alive(pid, None) {
                nxr_core::resource_guard::GroupLiveness::Dead => break,
                nxr_core::resource_guard::GroupLiveness::Alive
                | nxr_core::resource_guard::GroupLiveness::Uncertain => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        drop(admissions);
    });
}

fn decode_output(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::Operation;
    use nxr_core::diagnostics::exit;

    use super::*;
    use crate::plan_exec::{PlanNode, build_serial_plan};
    use crate::resources::NodeResources;
    use crate::scheduler::Scheduler;
    use crate::schema::TaskDefinition;

    fn plan_node_for_source(
        source_member: Option<String>,
        working_directory: Option<String>,
        environment: BTreeMap<String, String>,
    ) -> PlanNode {
        PlanNode {
            id: "node".to_owned(),
            depends_on: vec![],
            interactive: false,
            resources: NodeResources::default(),
            operation: None,
            platform: None,
            architecture: None,
            artifact_reuse: false,
            environment,
            working_directory,
            artifacts: vec![],
            inputs: vec![],
            cargo_output: false,
            source_member,
            remote_eligible: false,
        }
    }

    #[test]
    fn flake_and_nix_build_without_nix_are_unavailable_executor() {
        let empty = tempfile::tempdir().expect("empty path");
        let ctx = ExecContext::isolated(empty.path().display().to_string());
        let operations = [
            Operation::FlakeApp {
                name: "ci".to_owned(),
            },
            Operation::NixBuild {
                installable: ".#nxr".to_owned(),
            },
            Operation::WorkspaceScript {
                path: ".nxr/scripts/hello.sh".to_owned(),
                interpreter: None,
            },
        ];
        for operation in operations {
            let error = execute_operation(&operation, &ctx).expect_err("nix missing");
            assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
            assert_eq!(error.exit_code(), exit::NIX_CAPABILITY);
            assert!(matches!(
                error,
                ExecuteError::UnavailableExecutor {
                    executor: ExecutorName::Nix,
                    ..
                }
            ));
        }
    }

    #[test]
    fn flake_plan_node_without_nix_is_unavailable_executor() {
        let mut tasks = BTreeMap::new();
        tasks.insert("ci".to_owned(), TaskDefinition::new("ci"));
        let plan = build_serial_plan(&tasks, "ci").expect("plan");
        assert_eq!(
            plan.nodes[0].operation,
            Some(Operation::FlakeApp {
                name: "ci".to_owned()
            })
        );
        let ctx = ExecContext {
            path: tempfile::tempdir()
                .expect("empty path")
                .path()
                .display()
                .to_string(),
            nix_override: Some("/definitely/missing/nix".to_owned()),
            cwd: None,
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let error = run_operation_plan(&plan, &ctx, 1, None).expect_err("nix missing");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert_eq!(error.exit_code(), exit::NIX_CAPABILITY);
    }

    #[test]
    fn winbuild_without_binary_is_unavailable_executor() {
        let operation = Operation::WinBuildTask {
            task_id: "link-release".to_owned(),
            manifest: ".winbuild.json".to_owned(),
        };
        let ctx = ExecContext {
            path: tempfile::tempdir()
                .expect("empty path")
                .path()
                .display()
                .to_string(),
            nix_override: Some("/definitely/missing/nix".to_owned()),
            cwd: None,
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let error = execute_operation(&operation, &ctx).expect_err("winbuild missing");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert_eq!(error.exit_code(), exit::PROCESS_SUPERVISION);
        assert!(matches!(
            error,
            ExecuteError::UnavailableExecutor {
                executor: ExecutorName::Winbuild,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn winbuild_absent_does_not_start_wsl() {
        let trap_dir = tempfile::tempdir().expect("trap dir");
        let wsl_marker = trap_dir.path().join("wsl-ran");
        let nix_marker = trap_dir.path().join("nix-ran");
        write_trap(trap_dir.path(), "wsl", &wsl_marker);
        write_trap(trap_dir.path(), "nix", &nix_marker);
        let operation = Operation::WinBuildTask {
            task_id: "link-release".to_owned(),
            manifest: ".winbuild.json".to_owned(),
        };
        let ctx = ExecContext {
            path: trap_dir.path().display().to_string(),
            nix_override: Some("/definitely/missing/nix".to_owned()),
            cwd: None,
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let error = execute_operation(&operation, &ctx).expect_err("winbuild missing");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert!(!wsl_marker.exists(), "winbuild fallback must not start WSL");
        assert!(!nix_marker.exists(), "winbuild must not locate nix");
    }

    #[cfg(unix)]
    #[test]
    fn a_native_flake_app_fixture_runs_without_wsl() {
        let trap_dir = tempfile::tempdir().expect("trap dir");
        let wsl_marker = trap_dir.path().join("wsl-ran");
        let app_marker = trap_dir.path().join("app-ran");
        write_trap(trap_dir.path(), "wsl", &wsl_marker);
        write_trap(trap_dir.path(), "hello", &app_marker);
        let operation = Operation::NativeCommand {
            argv: vec!["hello".to_owned()],
        };
        let ctx = ExecContext {
            path: trap_dir.path().display().to_string(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let result = execute_operation(&operation, &ctx).expect("fixture app");
        assert!(matches!(result, OperationResult::Exited { code: 0, .. }));
        assert!(app_marker.is_file(), "the flake-app fixture must run");
        assert!(!wsl_marker.exists(), "the fixture must not start WSL");
    }

    #[test]
    fn remote_is_unavailable_without_dialing() {
        let operation = Operation::Remote {
            target: "builder".to_owned(),
            task: String::new(),
            source_url: String::new(),
            source_revision: String::new(),
            lock_digest: String::new(),
            source_ref: String::new(),
            lock_file: String::new(),
            build_root: String::new(),
            manifest: String::new(),
        };
        let error = execute_operation(&operation, &ExecContext::isolated("")).expect_err("remote");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert_eq!(error.exit_code(), exit::PROCESS_SUPERVISION);
    }

    #[test]
    fn scheduler_order_ignores_operation_payload() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("app-a"));
        let mut next = TaskDefinition::new("app-b");
        next.depends_on = vec!["a".to_owned()];
        tasks.insert("b".to_owned(), next);
        let mut plan = build_serial_plan(&tasks, "b").expect("plan");
        let flake_ready = Scheduler::new(&plan, 2)
            .expect("scheduler")
            .schedule_ready();
        for node in &mut plan.nodes {
            node.operation = Some(Operation::NativeCommand {
                argv: vec!["/bin/true".to_owned()],
            });
        }
        let native_ready = Scheduler::new(&plan, 2)
            .expect("scheduler")
            .schedule_ready();
        assert_eq!(flake_ready, vec!["a".to_owned()]);
        assert_eq!(flake_ready, native_ready);
    }

    #[test]
    fn source_member_without_mount_refuses_invoking_cwd() {
        let invoke = tempfile::tempdir().expect("invoke checkout");
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: Some(invoke.path().to_path_buf()),
            env: BTreeMap::new(),
            member_roots: BTreeMap::new(),
        };
        let node = plan_node_for_source(Some("stargate".to_owned()), None, BTreeMap::new());
        let error = context_for_node(&ctx, &node).expect_err("missing member root");
        assert!(matches!(
            error,
            ExecuteError::SourceUnresolved {
                member,
                detail
            } if member == "stargate" && detail == "source member is not mounted"
        ));
    }

    #[test]
    fn source_member_with_mount_selects_member_root() {
        let mount = tempfile::tempdir().expect("member root");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), mount.path().to_path_buf());
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: Some(PathBuf::from("/invoking-checkout")),
            env: BTreeMap::new(),
            member_roots,
        };
        let node = plan_node_for_source(
            Some("stargate".to_owned()),
            Some("pkg".to_owned()),
            BTreeMap::new(),
        );
        let resolved = context_for_node(&ctx, &node).expect("mounted member");
        assert_eq!(resolved.cwd, Some(mount.path().join("pkg")));
    }

    #[test]
    fn source_member_revision_mismatch_fails() {
        let mount = tempfile::tempdir().expect("member root");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), mount.path().to_path_buf());
        let mut env = BTreeMap::new();
        env.insert(
            "NXR_EXPECTED_REVISION_stargate".to_owned(),
            "abc".to_owned(),
        );
        env.insert(
            "NXR_OBSERVED_REVISION_stargate".to_owned(),
            "def".to_owned(),
        );
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: None,
            env,
            member_roots,
        };
        let node = plan_node_for_source(Some("stargate".to_owned()), None, BTreeMap::new());
        let error = context_for_node(&ctx, &node).expect_err("revision mismatch");
        assert!(matches!(
            error,
            ExecuteError::SourceUnresolved {
                member,
                detail
            } if member == "stargate" && detail == "source revision does not match"
        ));
    }

    #[test]
    fn source_member_revision_absent_does_not_fail() {
        let mount = tempfile::tempdir().expect("member root");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), mount.path().to_path_buf());
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots,
        };
        let node = plan_node_for_source(Some("stargate".to_owned()), None, BTreeMap::new());
        context_for_node(&ctx, &node).expect("no revision gate without expected key");
    }

    #[test]
    fn task_environment_cannot_satisfy_the_expected_revision() {
        let mount = tempfile::tempdir().expect("member root");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), mount.path().to_path_buf());
        let mut env = BTreeMap::new();
        env.insert(
            "NXR_EXPECTED_REVISION_stargate".to_owned(),
            "abc".to_owned(),
        );
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: None,
            env,
            member_roots,
        };
        let mut task_env = BTreeMap::new();
        task_env.insert(
            "NXR_EXPECTED_REVISION_stargate".to_owned(),
            "abc".to_owned(),
        );
        task_env.insert(
            "NXR_OBSERVED_REVISION_stargate".to_owned(),
            "abc".to_owned(),
        );
        let node = plan_node_for_source(Some("stargate".to_owned()), None, task_env);
        let error = context_for_node(&ctx, &node).expect_err("task env cannot attest revision");
        assert!(matches!(
            error,
            ExecuteError::SourceUnresolved { detail, .. } if detail == "source revision does not match"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn source_member_symlinked_root_fails() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().expect("parent");
        let target = tempfile::tempdir().expect("target");
        let link = parent.path().join("stargate");
        symlink(target.path(), &link).expect("symlink member root");
        let mut member_roots = BTreeMap::new();
        member_roots.insert("stargate".to_owned(), link);
        let ctx = ExecContext {
            path: String::new(),
            nix_override: None,
            cwd: None,
            env: BTreeMap::new(),
            member_roots,
        };
        let node = plan_node_for_source(Some("stargate".to_owned()), None, BTreeMap::new());
        let error = context_for_node(&ctx, &node).expect_err("symlink root");
        assert!(matches!(
            error,
            ExecuteError::SourceUnresolved {
                member,
                detail
            } if member == "stargate" && detail == "member root is a symlink"
        ));
    }

    #[test]
    fn artifact_outside_member_checkout_fails() {
        let root = tempfile::tempdir().expect("root");
        let checkout = root.path().join("member");
        std::fs::create_dir(&checkout).expect("member dir");
        std::fs::write(root.path().join("secret.txt"), b"outside").expect("outside file");
        let error = resolve_member_artifact(&checkout, "../secret.txt").expect_err("escapes");
        assert_eq!(error, ARTIFACT_ROOT_ESCAPE);
    }

    #[cfg(unix)]
    fn write_trap(dir: &std::path::Path, name: &str, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script = format!("#!/bin/sh\n/usr/bin/touch '{}'\n", marker.display());
        std::fs::write(&program, script).expect("write trap");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod trap");
    }
}
