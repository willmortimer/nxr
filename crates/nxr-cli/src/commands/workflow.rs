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
    plan_workflows, resolve_plan_pins,
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
        WorkflowSubcommand::Spec {
            name,
            source_set_out,
        } => print_execution_spec(name, source_set_out.as_deref()),
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
        WorkflowSubcommand::Attach {
            job_id,
            after_sequence,
        } => attach_native_build(job_id, *after_sequence),
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
    let Some(mut plan) = plan else {
        return Err(WorkflowError::new(
            exit::NOT_FOUND,
            "workflow file is not present",
        ));
    };
    let manifests = resolve_plan_pins(&root, &mut plan)
        .map_err(|error| WorkflowError::new(exit::USAGE, error))?;
    if dry_run {
        print_plan(&plan, json)?;
        if !manifests.is_empty() {
            let text = serde_json::to_string_pretty(&manifests).map_err(|error| {
                WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string())
            })?;
            println!("{text}");
        }
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

fn print_execution_spec(
    name: &str,
    source_set_out: Option<&std::path::Path>,
) -> Result<i32, WorkflowError> {
    let root = workflow_root()?;
    let path = nxr_task::workflows::workflow_path(&root);
    let bytes = std::fs::read(&path).map_err(|error| {
        WorkflowError::new(exit::NOT_FOUND, format!("read {}: {error}", path.display()))
    })?;
    let document = load_workflow_document(&root)
        .map_err(map_plan)?
        .ok_or_else(|| WorkflowError::new(exit::NOT_FOUND, "workflow file is not present"))?;
    let task = document.tasks.get(name).ok_or_else(|| {
        WorkflowError::new(exit::USAGE, format!("unknown workflow task `{name}`"))
    })?;
    let source = nxr_task::source_set_from_repo(&root)
        .map_err(|error| WorkflowError::new(exit::USAGE, error))?;
    let spec = nxr_task::execution_spec_for_task(
        &source,
        &nxr_task::workflow_definition_digest(&bytes),
        name,
        task,
    )
    .map_err(|error| WorkflowError::new(exit::USAGE, error))?;
    if let Some(path) = source_set_out {
        let source_text = serde_json::to_string_pretty(&source)
            .map_err(|error| WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string()))?;
        std::fs::write(path, source_text).map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("write {}: {error}", path.display()),
            )
        })?;
    }
    let text = serde_json::to_string_pretty(&spec)
        .map_err(|error| WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string()))?;
    println!("{text}");
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
        build_root,
        manifest,
        ..
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
    if !build_root.is_empty() {
        command.args(["--build-root", build_root]);
    }
    if !manifest.is_empty() {
        command.args(["--manifest", manifest]);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| ExecuteError::Spawn {
        detail: format!("failed to spawn nixplane native-build: {error}"),
    })?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let job_id = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let stderr_job = std::sync::Arc::clone(&job_id);
    let stderr_thread =
        std::thread::spawn(move || stream_pipe(stderr_pipe, &mut io::stderr(), true, &stderr_job));
    let stdout_thread = std::thread::spawn(move || {
        stream_pipe(
            stdout_pipe,
            &mut io::stdout(),
            false,
            &std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
    });
    let status = child.wait().map_err(|error| ExecuteError::Spawn {
        detail: format!("failed to wait for nixplane native-build: {error}"),
    })?;
    let stderr = stderr_thread.join().unwrap_or_default();
    let stdout = stdout_thread.join().unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal().is_some()
            && let Some(job) = job_id.lock().ok().and_then(|guard| guard.clone())
        {
            let _ = Command::new("nixplane")
                .args(["native-build", "cancel", "--job-id", &job])
                .status();
        }
    }
    Ok(OperationResult::Exited {
        code: status.code().unwrap_or(exit::CHILD_FAILED),
        stdout,
        stderr,
    })
}

fn stream_pipe(
    pipe: Option<impl std::io::Read>,
    sink: &mut impl Write,
    capture_job: bool,
    job_id: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> String {
    let Some(pipe) = pipe else {
        return String::new();
    };
    let mut captured = String::new();
    let reader = std::io::BufReader::new(pipe);
    for line in std::io::BufRead::lines(reader) {
        let Ok(line) = line else {
            break;
        };
        if capture_job
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(id) = value.get("job_id").and_then(|item| item.as_str())
            && let Ok(mut slot) = job_id.lock()
        {
            *slot = Some(id.to_owned());
        }
        let _ = writeln!(sink, "{line}");
        captured.push_str(&line);
        captured.push('\n');
    }
    captured
}

fn attach_native_build(job_id: &str, after_sequence: u64) -> Result<i32, WorkflowError> {
    let status = Command::new("nixplane")
        .args([
            "native-build",
            "attach",
            "--job-id",
            job_id,
            "--after-sequence",
            &after_sequence.to_string(),
        ])
        .status()
        .map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("failed to spawn nixplane native-build attach: {error}"),
            )
        })?;
    Ok(status.code().unwrap_or(exit::CHILD_FAILED))
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
            Event::StderrChunk { payload, .. } => {
                let _ = io::stderr().write_all(payload_bytes(&payload));
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
