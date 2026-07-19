//! `nxr task` execution (serial inherit or parallel supervised).

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{NixError, TaskDiscoveryError};
use nxr_process::{InterruptFlags, Supervisor};
use nxr_task::{
    Event, EventSink, ExecutionPlan, FailurePolicy, PlanError, Scheduler, SchedulerError,
    TaskDocument, build_execution_plan, resolve_task_name,
};

use crate::commands::common::{
    AppRequest, PrepareError, PreparedPlan, build_adapter, current_invocation_directory,
    prepare_app_plan,
};
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::run::RunError;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::output_task::{EventsFormat, TaskOutputMode, build_task_event_sink};
use crate::runner_output::RunnerOutput;

/// Grace window for Ctrl-C / fail-fast shutdown of in-flight children.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Poll interval while waiting for child exits / pipe chunks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Inputs for task execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub task: &'a str,
    /// Forwarded only to the root task's app ([`nxr_task::ArgumentForwarding::Root`]);
    /// dependency nodes always get none.
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub environment_policy: EnvironmentPolicy,
    /// Maximum concurrent running nodes (`-j` / `--jobs`; default 1).
    pub jobs: usize,
    /// When true, use [`FailurePolicy::KeepGoing`]; otherwise fail-fast.
    pub keep_going: bool,
    /// Parsed from global `--output`.
    pub output_mode: Option<TaskOutputMode>,
    /// Parsed from global `--events`.
    pub events_format: Option<EventsFormat>,
}

/// Errors while planning or running a task.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Discovery(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    PlanRender(#[from] PlanRenderError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("jobs must be >= 1 (got {0})")]
    InvalidJobs(usize),
    #[error("failed to supervise task children: {0}")]
    Supervision(#[source] io::Error),
    #[error("failed to write runner diagnostics: {0}")]
    Io(#[source] io::Error),
}

impl TaskError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Discovery(error) => error.exit_code(),
            Self::Plan(error) => plan_exit_code(error),
            Self::Run(error) => error.exit_code(),
            Self::PlanRender(_) => exit::EVALUATION,
            Self::Scheduler(_) | Self::Supervision(_) | Self::Io(_) => exit::PROCESS_SUPERVISION,
            Self::InvalidJobs(_) => exit::USAGE,
        }
    }
}

/// Map planner errors: unknown root is not-found; cycles/missing deps are graph errors.
#[must_use]
pub const fn plan_exit_code(error: &PlanError) -> i32 {
    match error {
        PlanError::UnknownRoot { .. } => exit::NOT_FOUND,
        PlanError::MissingDependency { .. } | PlanError::Cycle { .. } => exit::TASK_GRAPH,
    }
}

/// Discover tasks, build an execution plan, and run nodes under the scheduler.
///
/// # Argument forwarding (V2 freeze)
///
/// Trailing `args` are forwarded only to the **root** task's app
/// ([`ArgumentForwarding::Root`]). Dependency nodes always receive `[]`.
///
/// # Stdin policy
///
/// - **Inherit:** `jobs == 1` and neither `--output` nor `--events` is set
///   (serial interactive / transparent path).
/// - **Null/closed:** otherwise (`jobs > 1`, `--output`, or `--events`) for
///   every supervised child so parallel/multiplex runs never share caller stdin.
///
/// # Errors
///
/// Returns [`TaskError`] when flake resolution, discovery, planning, or app
/// preparation/supervision fails.
///
/// On success, returns the first nonzero child exit code (fail-fast or
/// keep-going), [`exit::INTERRUPTED`] after Ctrl-C cleanup, or `0` when every
/// required node succeeds / dry-run completes.
pub fn execute(
    request: &TaskRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, TaskError> {
    if request.jobs == 0 {
        return Err(TaskError::InvalidJobs(0));
    }

    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;
    let document = adapter.discover_tasks(&flake.nix_ref)?;

    let failure_policy = if request.keep_going {
        FailurePolicy::KeepGoing
    } else {
        FailurePolicy::FailFast
    };

    let canonical = resolve_task_name(&document, request.task)
        .map_err(|error| TaskError::Plan(PlanError::UnknownRoot { root: error.name }))?;

    let pipe_stdio = !task_inherits_stdin(request.jobs, request.output_mode, request.events_format);

    // Parallel runs without an explicit --output still need a labeled renderer so
    // piped child stdout is not discarded by NullSink.
    let effective_output = request.output_mode.or(if request.jobs > 1 {
        Some(TaskOutputMode::Live)
    } else {
        None
    });

    let plan = build_execution_plan(&document.tasks, canonical, failure_policy, None)?;

    let waves = parallel_ready_waves(&plan);
    let stdin_label = if pipe_stdio { "null" } else { "inherit" };
    runner
        .verbose(format!(
            "task plan for {canonical} (jobs={}, {}, args={}, stdin={}): {}",
            request.jobs,
            failure_policy.as_str(),
            plan.argument_forwarding.as_str(),
            stdin_label,
            format_wave_summary(&waves)
        ))
        .map_err(TaskError::Io)?;

    if dry_run {
        return dry_run_execute(request, &document, &plan, &waves, json, runner);
    }

    if pipe_stdio {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        let mut sink = build_task_event_sink(
            effective_output,
            request.events_format,
            &mut stdout,
            &mut stderr,
        );
        sink.emit(Event::PlanCreated {
            root: plan.root.clone(),
            node_count: plan.nodes.len(),
        });
        run_plan(request, &document, &plan, true, &mut sink, runner)
    } else {
        // Inherit stdio for interactivity: do not hold stdout/stderr locks.
        let mut sink = nxr_task::NullSink;
        run_plan(request, &document, &plan, false, &mut sink, runner)
    }
}

/// Serial interactive path inherits caller stdin; parallel/multiplex closes it.
///
/// Inherit when `jobs == 1` and neither `--output` nor `--events` is set.
/// Otherwise every supervised child gets null/closed stdin.
#[must_use]
pub fn task_inherits_stdin(
    jobs: usize,
    output_mode: Option<TaskOutputMode>,
    events_format: Option<EventsFormat>,
) -> bool {
    jobs == 1 && output_mode.is_none() && events_format.is_none()
}

fn dry_run_execute(
    request: &TaskRequest<'_>,
    document: &TaskDocument,
    plan: &ExecutionPlan,
    waves: &[Vec<String>],
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, TaskError> {
    let mut stdout = io::stdout().lock();
    let stdin_label =
        if task_inherits_stdin(request.jobs, request.output_mode, request.events_format) {
            "inherit"
        } else {
            "null"
        };
    writeln!(
        stdout,
        "# argument_forwarding={} stdin={}",
        plan.argument_forwarding.as_str(),
        stdin_label
    )
    .map_err(TaskError::Io)?;
    if request.jobs > 1 {
        writeln!(
            stdout,
            "# parallel schedule (jobs={}): {}",
            request.jobs,
            format_wave_summary(waves)
        )
        .map_err(TaskError::Io)?;
        for (index, wave) in waves.iter().enumerate() {
            writeln!(stdout, "# wave {}: {}", index + 1, wave.join(", ")).map_err(TaskError::Io)?;
        }
    } else {
        writeln!(
            stdout,
            "# serial schedule: {}",
            plan.serial_order.join(" -> ")
        )
        .map_err(TaskError::Io)?;
    }

    for task_id in &plan.serial_order {
        let prepared = prepare_node(request, document, &plan.root, task_id)?;
        runner
            .verbose(format!(
                "dry-run task {task_id} via app {}",
                prepared.plan.target
            ))
            .map_err(TaskError::Io)?;
        write_plan(&mut stdout, &prepared.plan, json)?;
    }

    Ok(exit::SUCCESS)
}

fn run_plan(
    request: &TaskRequest<'_>,
    document: &TaskDocument,
    plan: &ExecutionPlan,
    pipe_stdio: bool,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
) -> Result<i32, TaskError> {
    let mut scheduler = Scheduler::new(plan, request.jobs)?;
    let mut supervisor = Supervisor::new();
    let interrupts = InterruptFlags::install().map_err(TaskError::Supervision)?;
    let (io_tx, io_rx) = mpsc::channel::<IoChunk>();

    let mut first_failure: Option<i32> = None;
    let mut interrupted = false;

    let mut to_start = scheduler.schedule_ready();
    loop {
        if let Some(codes) = supervisor
            .handle_interrupt(&interrupts, SHUTDOWN_GRACE)
            .map_err(TaskError::Supervision)?
        {
            interrupted = true;
            for (id, code) in codes {
                sink.emit(Event::NodeExited {
                    node: id.clone(),
                    code: Some(code),
                });
                // Mark scheduler nodes complete so outcome is consistent; ignore
                // unknown ids (already reaped) by best-effort complete.
                let _ = scheduler.on_exit(&id, code);
            }
            break;
        }

        for node_id in to_start.drain(..) {
            spawn_node(
                request,
                document,
                &plan.root,
                &node_id,
                pipe_stdio,
                &mut supervisor,
                &io_tx,
                sink,
                runner,
            )?;
        }

        drain_io_chunks(&io_rx, sink, Duration::ZERO);

        if let Some((id, code)) = supervisor.try_wait_any().map_err(TaskError::Supervision)? {
            sink.emit(Event::NodeExited {
                node: id.clone(),
                code: Some(code),
            });

            if code != exit::SUCCESS && first_failure.is_none() {
                first_failure = Some(code);
            }

            to_start = scheduler.complete(&id, code)?;

            if scheduler.failure_policy() == FailurePolicy::FailFast
                && code != exit::SUCCESS
                && !supervisor.is_empty()
            {
                let shut = supervisor
                    .shutdown_all(SHUTDOWN_GRACE)
                    .map_err(TaskError::Supervision)?;
                for (stopped_id, stopped_code) in shut {
                    sink.emit(Event::NodeExited {
                        node: stopped_id.clone(),
                        code: Some(stopped_code),
                    });
                    let _ = scheduler.on_exit(&stopped_id, stopped_code);
                }
            }
            continue;
        }

        if scheduler.is_finished() && supervisor.is_empty() {
            break;
        }

        // Wait briefly for pipe data or a child exit.
        drain_io_chunks(&io_rx, sink, POLL_INTERVAL);
    }

    // Flush any trailing pipe chunks after the last exit.
    drain_io_chunks(&io_rx, sink, Duration::ZERO);

    let outcome = scheduler.outcome();
    let success = !interrupted && outcome.success;
    sink.emit(Event::RunCompleted { success });

    if interrupted {
        return Ok(exit::INTERRUPTED);
    }

    Ok(first_failure.unwrap_or(exit::SUCCESS))
}

#[allow(clippy::too_many_arguments)]
fn spawn_node(
    request: &TaskRequest<'_>,
    document: &TaskDocument,
    root_task_id: &str,
    node_id: &str,
    pipe_stdio: bool,
    supervisor: &mut Supervisor,
    io_tx: &Sender<IoChunk>,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
) -> Result<(), TaskError> {
    let prepared = prepare_node(request, document, root_task_id, node_id)?;
    runner
        .verbose(format!(
            "running task {node_id} via app {}",
            prepared.plan.target
        ))
        .map_err(TaskError::Io)?;

    sink.emit(Event::NodeStarted {
        node: node_id.to_owned(),
    });

    let program = prepared.nix.as_std_path();
    let args = &prepared.plan.command.arguments;
    let cwd = Some(prepared.execution_directory.as_std_path());
    let env = &prepared.plan.environment_policy;

    if pipe_stdio {
        // PipeStdoutStderr closes stdin (parallel/multiplex ownership policy).
        let (_pgid, stdout, stderr) = supervisor
            .spawn_piped(node_id.to_owned(), program, args, cwd, env)
            .map_err(TaskError::Supervision)?;
        spawn_pipe_reader(
            node_id.to_owned(),
            StreamKind::Stdout,
            stdout,
            io_tx.clone(),
        );
        spawn_pipe_reader(
            node_id.to_owned(),
            StreamKind::Stderr,
            stderr,
            io_tx.clone(),
        );
    } else {
        supervisor
            .spawn(node_id.to_owned(), program, args, cwd, env)
            .map_err(TaskError::Supervision)?;
    }

    Ok(())
}

fn prepare_node(
    request: &TaskRequest<'_>,
    document: &TaskDocument,
    root_task_id: &str,
    task_id: &str,
) -> Result<PreparedPlan, TaskError> {
    let definition = document
        .tasks
        .get(task_id)
        .expect("scheduler only starts ids present in the plan");
    // Compare against canonical root id (alias invocations still forward to root).
    let forwarded = if task_id == root_task_id {
        request.args
    } else {
        &[]
    };
    let app_request = AppRequest {
        flake_arg: request.flake_arg,
        nix_override: request.nix_override,
        app: definition.app.as_str(),
        args: forwarded,
        root: request.root,
        cwd: request.cwd,
        shell: request.shell,
        environment_policy: request.environment_policy.clone(),
    };
    Ok(prepare_app_plan(&app_request)?)
}

#[derive(Clone, Copy, Debug)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct IoChunk {
    node: String,
    kind: StreamKind,
    text: String,
}

fn spawn_pipe_reader(
    node: String,
    kind: StreamKind,
    mut reader: impl Read + Send + 'static,
    tx: Sender<IoChunk>,
) {
    thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if tx
                        .send(IoChunk {
                            node: node.clone(),
                            kind,
                            text,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
}

fn drain_io_chunks(rx: &Receiver<IoChunk>, sink: &mut dyn EventSink, timeout: Duration) {
    let mut deadline = if timeout.is_zero() {
        None
    } else {
        Some(std::time::Instant::now() + timeout)
    };

    loop {
        let chunk = if let Some(end) = deadline {
            let remaining = end.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                match rx.try_recv() {
                    Ok(chunk) => chunk,
                    Err(_) => break,
                }
            } else {
                match rx.recv_timeout(remaining) {
                    Ok(chunk) => chunk,
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
        } else {
            match rx.try_recv() {
                Ok(chunk) => chunk,
                Err(_) => break,
            }
        };

        // After the first timed wait, drain remaining without blocking.
        deadline = None;

        match chunk.kind {
            StreamKind::Stdout => sink.emit(Event::StdoutChunk {
                node: chunk.node,
                text: chunk.text,
            }),
            StreamKind::Stderr => sink.emit(Event::StderrChunk {
                node: chunk.node,
                text: chunk.text,
            }),
        }
    }
}

/// Compute ready-set waves assuming every node succeeds (for dry-run / verbose).
fn parallel_ready_waves(plan: &ExecutionPlan) -> Vec<Vec<String>> {
    let Ok(mut scheduler) = Scheduler::new(plan, usize::MAX) else {
        return plan
            .serial_order
            .iter()
            .cloned()
            .map(|id| vec![id])
            .collect();
    };

    let mut waves = Vec::new();
    while !scheduler.is_finished() {
        let started = scheduler.schedule_ready();
        if started.is_empty() {
            break;
        }
        waves.push(started.clone());
        for id in &started {
            let _ = scheduler.on_exit(id, 0);
        }
    }
    waves
}

fn format_wave_summary(waves: &[Vec<String>]) -> String {
    waves
        .iter()
        .map(|wave| {
            if wave.len() == 1 {
                wave[0].clone()
            } else {
                format!("[{}]", wave.join(" || "))
            }
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

#[cfg(test)]
mod tests {
    use super::{format_wave_summary, parallel_ready_waves, plan_exit_code, task_inherits_stdin};
    use crate::output_task::{EventsFormat, TaskOutputMode};
    use nxr_core::diagnostics::exit;
    use nxr_task::{FailurePolicy, PlanError, TaskDefinition, build_execution_plan};
    use std::collections::BTreeMap;

    #[test]
    fn unknown_root_maps_to_not_found() {
        let error = PlanError::UnknownRoot {
            root: "missing".to_owned(),
        };
        assert_eq!(plan_exit_code(&error), exit::NOT_FOUND);
    }

    #[test]
    fn cycle_maps_to_task_graph() {
        let error = PlanError::Cycle {
            path: vec!["a".to_owned(), "b".to_owned(), "a".to_owned()],
        };
        assert_eq!(plan_exit_code(&error), exit::TASK_GRAPH);
    }

    #[test]
    fn missing_dependency_maps_to_task_graph() {
        let error = PlanError::MissingDependency {
            task: "a".to_owned(),
            dependency: "ghost".to_owned(),
        };
        assert_eq!(plan_exit_code(&error), exit::TASK_GRAPH);
    }

    #[test]
    fn diamond_waves_run_siblings_together() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("a"));
        let mut b = TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        tasks.insert("b".to_owned(), b);
        let mut c = TaskDefinition::new("c");
        c.depends_on = vec!["a".to_owned()];
        tasks.insert("c".to_owned(), c);
        let mut d = TaskDefinition::new("d");
        d.depends_on = vec!["b".to_owned(), "c".to_owned()];
        tasks.insert("d".to_owned(), d);

        let plan = build_execution_plan(&tasks, "d", FailurePolicy::FailFast, None).expect("plan");
        let waves = parallel_ready_waves(&plan);
        assert_eq!(
            waves,
            vec![
                vec!["a".to_owned()],
                vec!["b".to_owned(), "c".to_owned()],
                vec!["d".to_owned()],
            ]
        );
        assert_eq!(format_wave_summary(&waves), "a -> [b || c] -> d");
    }

    #[test]
    fn serial_interactive_inherits_stdin() {
        assert!(task_inherits_stdin(1, None, None));
    }

    #[test]
    fn parallel_jobs_closes_stdin() {
        assert!(!task_inherits_stdin(2, None, None));
    }

    #[test]
    fn output_mode_closes_stdin() {
        assert!(!task_inherits_stdin(1, Some(TaskOutputMode::Live), None));
    }

    #[test]
    fn events_format_closes_stdin() {
        assert!(!task_inherits_stdin(1, None, Some(EventsFormat::Jsonl)));
    }
}
