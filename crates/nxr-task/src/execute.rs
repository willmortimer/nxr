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
use std::process::{Command, Stdio};

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
}

impl ExecutorName {
    /// Stable label used in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nix => "nix",
            Self::Winbuild => "winbuild",
            Self::Remote => "remote",
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
            Self::Winbuild | Self::Remote => exit::PROCESS_SUPERVISION,
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
            Self::MissingOperation { .. } => exit::TASK_GRAPH,
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
        Operation::WinBuildTask { task_id } => execute_winbuild(task_id, ctx),
        Operation::FlakeApp { .. }
        | Operation::NixBuild { .. }
        | Operation::WorkspaceScript { .. } => execute_nix_backed(ctx),
        Operation::Remote { target } => Err(ExecuteError::unavailable(
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
    mut sink: Option<&mut dyn EventSink>,
    mut nix: Option<
        &mut dyn FnMut(&Operation, &ExecContext) -> Result<OperationResult, ExecuteError>,
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
            if let Some(reason) =
                host_skip_reason(node.platform.as_deref(), node.architecture.as_deref())
            {
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
            let node_ctx = context_for_node(ctx, node);
            emit(sink.as_mut(), Event::node_queued(id.clone()));
            emit(sink.as_mut(), Event::node_started(id.clone()));
            let executed = match execute_operation(operation, &node_ctx) {
                Ok(OperationResult::NixPresent { program }) => match nix.as_mut() {
                    Some(dispatch) => dispatch(operation, &node_ctx),
                    None => Err(ExecuteError::NixNotInvoked { program }),
                },
                other => other,
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

fn context_for_node(base: &ExecContext, node: &PlanNode) -> ExecContext {
    let mut env = base.env.clone();
    env.extend(node.environment.clone());
    let cwd = match (&node.working_directory, &base.cwd) {
        (Some(relative), Some(root)) => Some(root.join(relative)),
        (Some(relative), None) => Some(PathBuf::from(relative)),
        (None, cwd) => cwd.clone(),
    };
    ExecContext {
        path: base.path.clone(),
        nix_override: base.nix_override.clone(),
        cwd,
        env,
    }
}

fn record_artifacts(
    node: &PlanNode,
    ctx: &ExecContext,
    id: &str,
    mut sink: Option<&mut &mut dyn EventSink>,
) -> Result<(), String> {
    for artifact in &node.artifacts {
        let path = match &ctx.cwd {
            Some(cwd) => cwd.join(artifact),
            None => PathBuf::from(artifact),
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
    }
    Ok(())
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

fn execute_winbuild(task_id: &str, ctx: &ExecContext) -> Result<OperationResult, ExecuteError> {
    // Search only for winbuild. Do not look up or spawn wsl.
    let Some(program) = find_program(&ctx.path, "winbuild") else {
        return Err(ExecuteError::unavailable(
            ExecutorName::Winbuild,
            "winbuild executable not found on PATH",
        ));
    };
    spawn_argv(&program, &[task_id.to_owned()], ctx)
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
    let mut command = Command::new(program);
    command.args(args);
    command.env_clear();
    command.env("PATH", &ctx.path);
    for (key, value) in &ctx.env {
        command.env(key, value);
    }
    if let Some(cwd) = &ctx.cwd {
        command.current_dir(cwd);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = command.output().map_err(|error| ExecuteError::Spawn {
        detail: format!("failed to spawn {}: {error}", program.display()),
    })?;
    let code = output.status.code().unwrap_or(exit::CHILD_FAILED);
    Ok(OperationResult::Exited {
        code,
        stdout: decode_output(&output.stdout),
        stderr: decode_output(&output.stderr),
    })
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
    use crate::plan_exec::build_serial_plan;
    use crate::scheduler::Scheduler;
    use crate::schema::TaskDefinition;

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
        };
        let error = run_operation_plan(&plan, &ctx, 1, None).expect_err("nix missing");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert_eq!(error.exit_code(), exit::NIX_CAPABILITY);
    }

    #[test]
    fn winbuild_without_binary_is_unavailable_executor() {
        let operation = Operation::WinBuildTask {
            task_id: "link-release".to_owned(),
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
        };
        let ctx = ExecContext {
            path: trap_dir.path().display().to_string(),
            nix_override: Some("/definitely/missing/nix".to_owned()),
            cwd: None,
            env: BTreeMap::new(),
        };
        let error = execute_operation(&operation, &ctx).expect_err("winbuild missing");
        assert_eq!(error.code(), UNAVAILABLE_EXECUTOR);
        assert!(!wsl_marker.exists(), "winbuild fallback must not start WSL");
        assert!(!nix_marker.exists(), "winbuild must not locate nix");
    }

    #[test]
    fn remote_is_unavailable_without_dialing() {
        let operation = Operation::Remote {
            target: "builder".to_owned(),
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

    #[cfg(unix)]
    fn write_trap(dir: &std::path::Path, name: &str, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        std::fs::write(&program, script).expect("write trap");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod trap");
    }
}
