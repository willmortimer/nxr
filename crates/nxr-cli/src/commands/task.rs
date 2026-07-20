//! `nxr task` execution (serial inherit or parallel supervised).

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_process::{InterruptFlags, Supervisor};
use nxr_task::{
    Event, EventSink, ExecutionPlan, FailurePolicy, OutputPayload, PlanError, Scheduler,
    SchedulerError, build_execution_plan_roots, resolve_task_name,
};

use crate::commands::common::{PrepareError, PreparedTaskNode, WorkspaceSnapshot};
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::run::RunError;
use crate::flake::FlakeResolveError;
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
    /// One or more task roots whose dependency subgraphs are unioned.
    pub tasks: &'a [String],
    /// Forwarded only to each root task's app ([`nxr_task::ArgumentForwarding::Root`]);
    /// dependency nodes always get none.
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_mode: crate::shell_mode::ShellMode,
    pub environment_policy: EnvironmentPolicy,
    /// Maximum concurrent running nodes (`-j` / `--jobs`; default 1).
    pub jobs: usize,
    /// When true, use [`FailurePolicy::KeepGoing`]; otherwise fail-fast.
    pub keep_going: bool,
    /// Parsed from global `--output`.
    pub output_mode: Option<TaskOutputMode>,
    /// Parsed from global `--events`.
    pub events_format: Option<EventsFormat>,
    pub nix_flags: &'a OptionalNixFlags,
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
    #[error(
        "--output raw requires -j 1 and cannot be combined with --events (raw inherits child stdio)"
    )]
    RawConflictsWithMultiplex,
    #[error(
        "interactive tasks cannot be combined with multiplexed --output or --events (interactive nodes inherit stdin/terminal)"
    )]
    InteractiveConflictsWithMultiplex,
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
            Self::InvalidJobs(_)
            | Self::RawConflictsWithMultiplex
            | Self::InteractiveConflictsWithMultiplex => exit::USAGE,
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

/// Discover tasks once, prepare every node, then run under the scheduler.
///
/// Flow: resolve flake → detect system once → evaluate tasks once → discover
/// apps once → validate referenced apps → construct every node plan → schedule
/// → execute prepared plans without further discovery/system detection.
///
/// # Argument forwarding (V2 freeze)
///
/// Trailing `args` are forwarded only to the **root** task's app
/// ([`ArgumentForwarding::Root`]). Dependency nodes always receive `[]`.
///
/// # Stdin policy
///
/// - **Inherit:** `jobs == 1` and neither multiplexed `--output` nor `--events`
///   is set (serial interactive / `--output raw` passthrough).
/// - **Null/closed:** otherwise (`jobs > 1`, multiplexed `--output`, or
///   `--events`) for every supervised child so parallel/multiplex runs never
///   share caller stdin.
///
/// `--output raw` inherits child stdio for a single foreground job stream and
/// conflicts with `-j > 1` and `--events`.
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
    execute_with_control(request, dry_run, json, runner, &mut || {
        Ok(RunControl::Continue)
    })
}

/// External control signals for watch-mode integration with the scheduler loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunControl {
    /// Keep running the current generation.
    Continue,
    /// Filesystem change — shut down children and restart.
    Restart,
    /// Cooperative stop (e.g. Ctrl-C observed by the outer watch loop).
    Stop,
}

/// Like [`execute`], but polls `control` during the scheduler loop.
///
/// Used by `nxr watch` / `task --watch` so mid-run filesystem changes can abort
/// the current generation and rebuild the snapshot/plan.
///
/// # Errors
///
/// Same as [`execute`]. Control-poll I/O errors map to [`TaskError::Supervision`].
pub fn execute_with_control(
    request: &TaskRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
    control: &mut dyn FnMut() -> io::Result<RunControl>,
) -> Result<i32, TaskError> {
    if request.jobs == 0 {
        return Err(TaskError::InvalidJobs(0));
    }

    if matches!(request.output_mode, Some(TaskOutputMode::Raw))
        && (request.jobs > 1 || request.events_format.is_some())
    {
        return Err(TaskError::RawConflictsWithMultiplex);
    }

    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        true,
        request.nix_flags,
    )?;
    let document = snapshot
        .tasks
        .as_ref()
        .expect("load_tasks=true always populates tasks")
        .clone();

    let failure_policy = if request.keep_going {
        FailurePolicy::KeepGoing
    } else {
        FailurePolicy::FailFast
    };

    let canonical_roots: Vec<String> = request
        .tasks
        .iter()
        .map(|name| {
            resolve_task_name(&document, name)
                .map(str::to_owned)
                .map_err(|error| TaskError::Plan(PlanError::UnknownRoot { root: error.name }))
        })
        .collect::<Result<_, _>>()?;
    let root_refs: Vec<&str> = canonical_roots.iter().map(String::as_str).collect();

    let plan = build_execution_plan_roots(&document.tasks, &root_refs, failure_policy, None)?;
    validate_interactive_run(&plan, request)?;
    snapshot
        .validate_task_apps(&document)
        .map_err(PrepareError::NotFound)?;
    let prepared_nodes = snapshot.prepare_task_nodes(
        &document,
        &canonical_roots,
        &plan.serial_order,
        request.args,
        request.root,
        request.cwd,
        request.shell,
        request.shell_mode,
        &request.environment_policy,
        request.nix_flags,
    )?;

    // Parallel runs without an explicit --output still need a labeled renderer so
    // piped child stdout is not discarded by NullSink.
    let effective_output = request.output_mode.or(if request.jobs > 1 {
        Some(TaskOutputMode::Live)
    } else {
        None
    });

    let waves = parallel_ready_waves(&plan, request.jobs);
    let pipe_stdio = plan_uses_piped_stdio(&plan, request);
    log_task_plan_verbose(
        &format_task_roots(&canonical_roots),
        &plan,
        request,
        failure_policy,
        &waves,
        runner,
    )?;

    if dry_run {
        return dry_run_execute(&prepared_nodes, &plan, &waves, request, json, runner);
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
            roots: if plan.roots.is_empty() {
                None
            } else {
                Some(plan.roots.clone())
            },
            node_count: plan.nodes.len(),
        });
        run_plan(request, &plan, &prepared_nodes, &mut sink, runner, control)
    } else {
        // Inherit stdio for interactivity / --output raw: do not hold stdout/stderr locks.
        let mut sink = nxr_task::NullSink;
        run_plan(request, &plan, &prepared_nodes, &mut sink, runner, control)
    }
}

/// Reject multiplex output/events when the plan contains interactive nodes.
fn validate_interactive_run(
    plan: &ExecutionPlan,
    request: &TaskRequest<'_>,
) -> Result<(), TaskError> {
    if plan.has_interactive_nodes()
        && (request.events_format.is_some()
            || matches!(request.output_mode, Some(mode) if mode.is_multiplexed()))
    {
        return Err(TaskError::InteractiveConflictsWithMultiplex);
    }
    Ok(())
}

fn log_task_plan_verbose(
    canonical: &str,
    plan: &ExecutionPlan,
    request: &TaskRequest<'_>,
    failure_policy: FailurePolicy,
    waves: &[Vec<String>],
    runner: RunnerOutput,
) -> Result<(), TaskError> {
    let pipe_stdio = plan_uses_piped_stdio(plan, request);
    let stdin_label = if plan.has_interactive_nodes() {
        "inherit (interactive)"
    } else if pipe_stdio {
        "null"
    } else {
        "inherit"
    };
    runner
        .verbose(format!(
            "task plan for {canonical} (jobs={}, {}, args={}, stdin={}): {}",
            request.jobs,
            failure_policy.as_str(),
            plan.argument_forwarding.as_str(),
            stdin_label,
            format_wave_summary(waves)
        ))
        .map_err(TaskError::Io)?;
    if plan.has_interactive_nodes() {
        let interactive = plan.interactive_node_ids().collect::<Vec<_>>().join(", ");
        runner
            .verbose(format!(
                "interactive exclusivity: nodes [{interactive}] run alone (stdin/terminal inherited; no concurrent peers)"
            ))
            .map_err(TaskError::Io)?;
    }
    Ok(())
}

fn node_is_interactive(plan: &ExecutionPlan, node_id: &str) -> bool {
    plan.nodes
        .iter()
        .find(|node| node.id == node_id)
        .is_some_and(|node| node.interactive)
}

/// Serial interactive and `--output raw` paths inherit caller stdin; multiplex closes it.
///
/// Inherit when `jobs == 1`, `--events` is unset, and `--output` is absent or `raw`.
/// Otherwise every supervised child gets null/closed stdin.
#[must_use]
pub fn task_inherits_stdin(
    jobs: usize,
    output_mode: Option<TaskOutputMode>,
    events_format: Option<EventsFormat>,
) -> bool {
    if jobs != 1 || events_format.is_some() {
        return false;
    }
    match output_mode {
        None | Some(TaskOutputMode::Raw) => true,
        Some(_) => false,
    }
}

/// Whether any node in `plan` uses piped stdio under `request`.
#[must_use]
pub fn plan_uses_piped_stdio(plan: &ExecutionPlan, request: &TaskRequest<'_>) -> bool {
    plan.nodes.iter().any(|node| {
        node_uses_piped_stdio(
            node.interactive,
            request.jobs,
            request.output_mode,
            request.events_format,
        )
    })
}

/// Per-node stdio policy: interactive nodes always inherit; others follow serial/multiplex rules.
#[must_use]
pub fn node_uses_piped_stdio(
    interactive: bool,
    jobs: usize,
    output_mode: Option<TaskOutputMode>,
    events_format: Option<EventsFormat>,
) -> bool {
    if interactive {
        return false;
    }
    !task_inherits_stdin(jobs, output_mode, events_format)
}

fn dry_run_execute(
    prepared_nodes: &BTreeMap<String, PreparedTaskNode>,
    plan: &ExecutionPlan,
    waves: &[Vec<String>],
    request: &TaskRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, TaskError> {
    let mut stdout = io::stdout().lock();
    let stdin_label = if plan.has_interactive_nodes() {
        "inherit (interactive)"
    } else if plan_uses_piped_stdio(plan, request) {
        "null"
    } else {
        "inherit"
    };
    writeln!(
        stdout,
        "# argument_forwarding={} stdin={}",
        plan.argument_forwarding.as_str(),
        stdin_label
    )
    .map_err(TaskError::Io)?;
    if plan.has_interactive_nodes() {
        let interactive = plan.interactive_node_ids().collect::<Vec<_>>().join(", ");
        writeln!(
            stdout,
            "# interactive_exclusivity: nodes [{interactive}] run alone (stdin/terminal inherited)"
        )
        .map_err(TaskError::Io)?;
    }
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
        let prepared = prepared_nodes
            .get(task_id)
            .expect("every serial_order id was prepared before dry-run");
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

#[allow(clippy::too_many_lines)]
fn run_plan(
    request: &TaskRequest<'_>,
    plan: &ExecutionPlan,
    prepared_nodes: &BTreeMap<String, PreparedTaskNode>,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
    control: &mut dyn FnMut() -> io::Result<RunControl>,
) -> Result<i32, TaskError> {
    let mut scheduler = Scheduler::new(plan, request.jobs)?;
    let mut supervisor = Supervisor::new();
    let interrupts = InterruptFlags::install().map_err(TaskError::Supervision)?;
    let (io_tx, io_rx) = mpsc::channel::<IoChunk>();

    let mut first_failure: Option<i32> = None;
    let mut interrupted = false;
    let mut restarted = false;

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

        match control().map_err(TaskError::Supervision)? {
            RunControl::Continue => {}
            signal @ (RunControl::Restart | RunControl::Stop) => {
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
                match signal {
                    RunControl::Restart => restarted = true,
                    RunControl::Stop => interrupted = true,
                    RunControl::Continue => {}
                }
                break;
            }
        }

        for node_id in to_start.drain(..) {
            let pipe_stdio = node_uses_piped_stdio(
                node_is_interactive(plan, &node_id),
                request.jobs,
                request.output_mode,
                request.events_format,
            );
            spawn_node(
                prepared_nodes,
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
    let success = !interrupted && !restarted && outcome.success;
    sink.emit(Event::RunCompleted { success });

    if interrupted {
        return Ok(exit::INTERRUPTED);
    }

    // Watch restart: treat as success for this generation so the outer loop
    // rebuilds; the caller detects Restart via its control flag.
    if restarted {
        return Ok(exit::SUCCESS);
    }

    Ok(first_failure.unwrap_or(exit::SUCCESS))
}

fn spawn_node(
    prepared_nodes: &BTreeMap<String, PreparedTaskNode>,
    node_id: &str,
    pipe_stdio: bool,
    supervisor: &mut Supervisor,
    io_tx: &Sender<IoChunk>,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
) -> Result<(), TaskError> {
    let prepared = prepared_nodes
        .get(node_id)
        .expect("scheduler only starts ids prepared before run");
    runner
        .verbose(format!(
            "running task {node_id} via app {}",
            prepared.plan.target
        ))
        .map_err(TaskError::Io)?;

    sink.emit(Event::NodeStarted {
        node: node_id.to_owned(),
    });

    let program = prepared.program.as_std_path();
    let args = &prepared.arguments;
    let cwd = Some(prepared.cwd.as_std_path());
    let env = &prepared.environment;

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

#[derive(Clone, Copy, Debug)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct IoChunk {
    node: String,
    kind: StreamKind,
    bytes: Vec<u8>,
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
                    // Preserve raw bytes; UTF-8 decoding belongs to human renderers.
                    if tx
                        .send(IoChunk {
                            node: node.clone(),
                            kind,
                            bytes: buf[..n].to_vec(),
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

        let payload = OutputPayload::from_bytes(chunk.bytes);
        match chunk.kind {
            StreamKind::Stdout => sink.emit(Event::StdoutChunk {
                node: chunk.node,
                payload,
            }),
            StreamKind::Stderr => sink.emit(Event::StderrChunk {
                node: chunk.node,
                payload,
            }),
        }
    }
}

/// Compute ready-set waves assuming every node succeeds (for dry-run / verbose).
fn parallel_ready_waves(plan: &ExecutionPlan, jobs: usize) -> Vec<Vec<String>> {
    let Ok(mut scheduler) = Scheduler::new(plan, jobs.max(1)) else {
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

fn format_task_roots(roots: &[String]) -> String {
    roots.join("+")
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
    use super::{
        format_wave_summary, node_uses_piped_stdio, parallel_ready_waves, plan_exit_code,
        plan_uses_piped_stdio, task_inherits_stdin,
    };
    use crate::output_task::{EventsFormat, TaskOutputMode};
    use nxr_core::EnvironmentPolicy;
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
        let waves = parallel_ready_waves(&plan, 2);
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
    fn raw_output_inherits_stdin() {
        assert!(task_inherits_stdin(1, Some(TaskOutputMode::Raw), None));
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
    fn raw_with_parallel_jobs_closes_stdin() {
        assert!(!task_inherits_stdin(2, Some(TaskOutputMode::Raw), None));
    }

    #[test]
    fn events_format_closes_stdin() {
        assert!(!task_inherits_stdin(1, None, Some(EventsFormat::Jsonl)));
    }

    #[test]
    fn interactive_node_never_uses_piped_stdio() {
        assert!(!node_uses_piped_stdio(
            true,
            2,
            Some(TaskOutputMode::Live),
            None
        ));
        assert!(!node_uses_piped_stdio(true, 2, None, None));
    }

    #[test]
    fn interactive_siblings_serialize_waves_with_jobs_2() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("a"));
        let mut b = TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        b.interactive = true;
        tasks.insert("b".to_owned(), b);
        let mut c = TaskDefinition::new("c");
        c.depends_on = vec!["a".to_owned()];
        tasks.insert("c".to_owned(), c);
        let mut d = TaskDefinition::new("d");
        d.depends_on = vec!["b".to_owned(), "c".to_owned()];
        tasks.insert("d".to_owned(), d);

        let plan = build_execution_plan(&tasks, "d", FailurePolicy::FailFast, None).expect("plan");
        let waves = parallel_ready_waves(&plan, 2);
        assert_eq!(
            waves,
            vec![
                vec!["a".to_owned()],
                vec!["b".to_owned()],
                vec!["c".to_owned()],
                vec!["d".to_owned()],
            ]
        );
    }

    #[test]
    fn plan_with_interactive_uses_piped_stdio_for_parallel_non_interactive() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("a"));
        let mut b = TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        b.interactive = true;
        tasks.insert("b".to_owned(), b);
        let plan = build_execution_plan(&tasks, "b", FailurePolicy::FailFast, None).expect("plan");
        let nix_flags = nxr_nix::OptionalNixFlags::default();
        let task_names = vec!["b".to_owned()];
        let request = super::TaskRequest {
            flake_arg: None,
            nix_override: None,
            tasks: &task_names,
            args: &[],
            root: false,
            cwd: None,
            shell: None,
            shell_mode: crate::shell_mode::ShellMode::Smart,
            environment_policy: EnvironmentPolicy::Inherit,
            jobs: 2,
            keep_going: false,
            output_mode: None,
            events_format: None,
            nix_flags: &nix_flags,
        };
        assert!(plan_uses_piped_stdio(&plan, &request));
    }
}
