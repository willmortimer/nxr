//! `nxr workflow` and `nxr task workflow[:name]`.
//!
//! Flake executors go through `nix run`. Native and winbuild executors stay on
//! the operation runner and do not look up Nix.

use std::io::{self, Write};
use std::process::{Command, Stdio};

use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, Operation};
use nxr_task::events::{Event, EventSink, NodeOutcome, OutputPayload};
use nxr_task::execute::{ExecContext, ExecuteError, OperationResult, run_operation_plan_callbacks};
use nxr_task::plan_exec::{ExecutionPlan, FailurePolicy};
use nxr_task::workflows::{
    WorkflowError as PlanError, WorkflowPlanMode, find_workflow_root, load_workflow_document,
    plan_workflows,
};

use crate::cli::WorkflowSubcommand;
use crate::commands::common::{current_invocation_directory, locate_nix_path};

/// Failure while listing, planning, or running a workflow file.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// The command cannot continue.
    #[error("{message}")]
    Message {
        /// Process status.
        code: i32,
        /// Already safe to print.
        message: String,
    },
}

impl WorkflowError {
    /// Existing public exit status.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Message { code, .. } => *code,
        }
    }

    fn new(code: i32, message: impl Into<String>) -> Self {
        Self::Message {
            code,
            message: message.into(),
        }
    }
}

/// Run a `nxr workflow` subcommand.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the file is missing or the graph is invalid.
/// A task that exits nonzero is `Ok(code)`, not an error.
pub fn execute(
    action: &WorkflowSubcommand,
    nix: Option<&str>,
    dry_run: bool,
    json: bool,
) -> Result<i32, WorkflowError> {
    match action {
        WorkflowSubcommand::List => list_tasks(),
        WorkflowSubcommand::Plan { name } => {
            let mode = WorkflowPlanMode::Explicit { root: name.clone() };
            execute_mode(&mode, 1, false, nix, true, json)
        }
        WorkflowSubcommand::Run {
            name,
            jobs,
            keep_going,
        } => {
            let mode = WorkflowPlanMode::Explicit {
                root: Some(name.clone()),
            };
            execute_mode(&mode, *jobs, *keep_going, nix, dry_run, json)
        }
    }
}

/// Run an explicit workflow selector from `nxr task`.
///
/// # Errors
///
/// Returns [`WorkflowError`] for a missing file or an invalid graph.
pub fn execute_mode(
    mode: &WorkflowPlanMode,
    jobs: usize,
    keep_going: bool,
    nix: Option<&str>,
    dry_run: bool,
    json: bool,
) -> Result<i32, WorkflowError> {
    let root = workflow_root()?;
    let policy = if keep_going {
        FailurePolicy::KeepGoing
    } else {
        FailurePolicy::FailFast
    };
    let plan = plan_workflows(&root, mode.clone(), policy, None).map_err(map_plan)?;
    let Some(plan) = plan else {
        return Err(WorkflowError::new(
            exit::NOT_FOUND,
            "workflow file is not present",
        ));
    };
    if dry_run {
        print_plan(&plan, json)?;
        return Ok(exit::SUCCESS);
    }
    let mut sink = StdioSink;
    let ctx = execution_context(&root, nix);
    let run = run_operation_plan_callbacks(
        &plan,
        &ctx,
        jobs.max(1),
        Some(&mut sink),
        Some(&mut nix_run),
        Some(&mut remote_run),
    )
    .map_err(map_execute)?;
    Ok(run.exit_code)
}

fn list_tasks() -> Result<i32, WorkflowError> {
    let root = workflow_root()?;
    let document = load_workflow_document(&root)
        .map_err(map_plan)?
        .ok_or_else(|| WorkflowError::new(exit::NOT_FOUND, "workflow file is not present"))?;
    for (name, task) in &document.tasks {
        let platform = task
            .platform
            .as_plan_label()
            .unwrap_or_else(|| "any".to_owned());
        println!("{name}\t{platform}\t{}", task.operation.kind());
    }
    Ok(exit::SUCCESS)
}

fn workflow_root() -> Result<std::path::PathBuf, WorkflowError> {
    let cwd = current_invocation_directory()
        .map_err(|error| WorkflowError::new(exit::DISCOVERY, error.to_string()))?;
    find_workflow_root(cwd.as_std_path()).ok_or_else(|| {
        WorkflowError::new(
            exit::NOT_FOUND,
            "no .nxr/workflows.toml found from the current directory",
        )
    })
}

fn execution_context(root: &std::path::Path, nix: Option<&str>) -> ExecContext {
    let mut ctx = ExecContext::isolated(std::env::var("PATH").unwrap_or_default());
    ctx.cwd = Some(root.to_path_buf());
    if let Ok(path) = locate_nix_path(nix) {
        ctx.nix_override = Some(path.into_string());
    }
    ctx
}

fn print_plan(plan: &ExecutionPlan, json: bool) -> Result<(), WorkflowError> {
    if json {
        let text = serde_json::to_string_pretty(plan)
            .map_err(|error| WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string()))?;
        println!("{text}");
        return Ok(());
    }
    println!("root: {}", plan.root);
    for (index, wave) in plan.waves.iter().enumerate() {
        println!("wave {index}: {}", wave.join(" "));
    }
    Ok(())
}

fn remote_run(operation: &Operation, _ctx: &ExecContext) -> Result<OperationResult, ExecuteError> {
    let Operation::Remote {
        target,
        task,
        source_url,
        source_revision,
        lock_digest,
    } = operation
    else {
        return Err(ExecuteError::UnavailableExecutor {
            executor: nxr_task::execute::ExecutorName::Remote,
            detail: "remote executor is not available".to_owned(),
        });
    };
    let mut command = Command::new("nixplane");
    command.args([
        "native-build",
        "run",
        "--target",
        target,
        "--task",
        task,
        "--source-url",
        source_url,
        "--source-revision",
        source_revision,
        "--lock-digest",
        lock_digest,
    ]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = command.output().map_err(|error| ExecuteError::Spawn {
        detail: format!("failed to spawn nixplane native-build: {error}"),
    })?;
    Ok(OperationResult::Exited {
        code: output.status.code().unwrap_or(exit::CHILD_FAILED),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn nix_run(operation: &Operation, ctx: &ExecContext) -> Result<OperationResult, ExecuteError> {
    let Operation::FlakeApp { name } = operation else {
        return Err(ExecuteError::NixNotInvoked {
            program: ctx.nix_override.clone().unwrap_or_else(|| "nix".to_owned()),
        });
    };
    let program = ctx.nix_override.clone().unwrap_or_else(|| "nix".to_owned());
    let mut command = Command::new(&program);
    command.arg("run").arg(format!(".#{name}"));
    EnvironmentPolicy::Inherit.apply(&mut command);
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
        detail: format!("failed to spawn nix run: {error}"),
    })?;
    Ok(OperationResult::Exited {
        code: output.status.code().unwrap_or(exit::CHILD_FAILED),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn map_plan(error: PlanError) -> WorkflowError {
    WorkflowError::new(error.exit_code(), error.to_string())
}

fn map_execute(error: ExecuteError) -> WorkflowError {
    WorkflowError::new(error.exit_code(), error.to_string())
}

struct StdioSink;

impl EventSink for StdioSink {
    fn emit(&mut self, event: Event) {
        match event {
            Event::Diagnostic { message } => {
                eprintln!("{message}");
            }
            Event::StdoutChunk { payload, .. } => {
                let _ = io::stdout().write_all(payload_bytes(&payload));
            }
            Event::NodeExited {
                node,
                status: Some(NodeOutcome::Failed),
                code,
                reason,
                ..
            } => {
                if let Some(reason) = reason {
                    eprintln!("workflow `{node}` failed: {reason}");
                } else {
                    eprintln!("workflow `{node}` failed ({code:?})");
                }
            }
            _ => {}
        }
    }
}

fn payload_bytes(payload: &OutputPayload) -> &[u8] {
    payload.as_bytes()
}
