//! `nxr task` execution (serial inherit or parallel supervised).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Duration;

use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, PlanSecretRef};
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_process::{DeadlineQueue, InterruptFlags, PipeMultiplexer, PipeStream, Supervisor};
use nxr_task::{
    ContextError, Event, EventSink, ExecutionPlan, FailurePolicy, OutputPayload, PlanError,
    PlanSecretEntry, PlanSecretValuePlaceholder, RunEventDecorator, Scheduler, SchedulerError,
    SecretDelivery, SecretProvider, build_execution_plan_roots, enforce_context_confirm,
    merge_spawn_env_overrides, resolve_task_name,
};

use crate::commands::common::{
    NodePrepStage, PrepareError, PreparedTaskNode, TaskNodePreparer, WorkspaceSnapshot,
    WorkspaceState, lazy_prep_enabled,
};
use crate::commands::history;
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::run::RunError;
use crate::commands::secrets::{
    SpawnSecrets, load_runtime_secret_config, prepare_spawn_secrets, project_identity,
};
use crate::commands::trust;
use crate::commands::workspace_cache::{
    explain_workspace_cache, save_workspace_cache, try_workspace_cache_restore,
};
use crate::flake::FlakeResolveError;
use crate::output_task::{EventsFormat, TaskOutputMode, build_task_event_sink};
use crate::reports::{ReportCollector, ReportPaths, ReportWriteError, write_all_reports};
use crate::runner_output::RunnerOutput;

/// Grace window for Ctrl-C / fail-fast shutdown of in-flight children.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Poll interval while waiting for child exits / pipe chunks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Whether [`drain_pipe_chunks`] should treat poll errors as fatal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipeDrainMode {
    Normal,
    /// Forced shutdown may race with mio fd teardown; `Interrupted` is benign then.
    ForcedShutdown,
}

/// Inputs for task execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    /// One or more task roots whose dependency subgraphs are unioned.
    pub tasks: Vec<String>,
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
    /// Opt-in post-run report output paths.
    pub reports: ReportPaths,
    pub nix_flags: &'a OptionalNixFlags,
    /// When set, overrides each task's declared `context` field.
    pub context_override: Option<String>,
    /// Bypass nxr discovery cache for this invocation.
    pub refresh_discovery: bool,
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
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error(transparent)]
    Trust(#[from] trust::TrustCommandError),
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
    #[error(transparent)]
    Report(#[from] ReportWriteError),
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
            Self::Scheduler(_) | Self::Supervision(_) | Self::Io(_) | Self::Report(_) => {
                exit::PROCESS_SUPERVISION
            }
            Self::Context(_) | Self::Trust(_) => exit::EVALUATION,
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
    let started = std::time::Instant::now();
    let code = execute_with_control(
        request,
        dry_run,
        json,
        runner,
        &mut || Ok(RunControl::Continue),
        None,
        false,
        None,
    )?;
    if !dry_run {
        let mut state = WorkspaceState::with_refresh(
            request.flake_arg,
            request.nix_override,
            request.nix_flags,
            request.refresh_discovery,
        );
        let discovery_context = state.discovery_context().ok();
        history::record_completed_run(
            started,
            nxr_core::RunTargetKind::Task,
            request.tasks.join(","),
            discovery_context.as_ref().map(|ctx| ctx.flake_ref.clone()),
            code,
            discovery_context.as_ref(),
            true,
        );
    }
    Ok(code)
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

/// Prepared task graph reused across source-only watch generations.
#[derive(Clone, Debug)]
pub struct PreparedTaskGeneration {
    pub document: nxr_task::TaskDocument,
    pub plan: ExecutionPlan,
    /// Fully populated for dry-run, watch reuse, and `NXR_LAZY_PREP=off`.
    /// Empty when [`Self::snapshot`] is set for lazy preparation.
    pub prepared_nodes: BTreeMap<String, PreparedTaskNode>,
    pub canonical_roots: Vec<String>,
    /// Workspace snapshot retained for lazy node prepare (ADR-0158).
    pub snapshot: Option<WorkspaceSnapshot>,
}

/// Like [`execute`], but polls `control` during the scheduler loop.
///
/// Used by `nxr watch` / `task --watch` so mid-run filesystem changes can abort
/// the current generation and rebuild the snapshot/plan.
///
/// # Errors
///
/// Same as [`execute`]. Control-poll I/O errors map to [`TaskError::Supervision`].
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn execute_with_control(
    request: &TaskRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
    control: &mut dyn FnMut() -> io::Result<RunControl>,
    workspace: Option<&mut WorkspaceState<'_>>,
    reuse_from_cache: bool,
    cache: Option<&mut Option<PreparedTaskGeneration>>,
) -> Result<i32, TaskError> {
    if request.jobs == 0 {
        return Err(TaskError::InvalidJobs(0));
    }

    if matches!(request.output_mode, Some(TaskOutputMode::Raw))
        && (request.jobs > 1 || request.events_format.is_some())
    {
        return Err(TaskError::RawConflictsWithMultiplex);
    }

    let prepared_bundle = if reuse_from_cache {
        cache
            .as_ref()
            .and_then(|cached| cached.as_ref())
            .cloned()
            .ok_or_else(|| {
                TaskError::Supervision(io::Error::other(
                    "watch reuse requested without a prepared task cache",
                ))
            })?
    } else {
        let owned_snapshot;
        let snapshot = if let Some(state) = workspace {
            state.snapshot(true).map_err(TaskError::Prepare)?
        } else {
            owned_snapshot = WorkspaceSnapshot::load_with_refresh(
                request.flake_arg,
                request.nix_override,
                true,
                request.nix_flags,
                request.refresh_discovery,
            )?;
            &owned_snapshot
        };
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

        // Watch caches and dry-run need every node prepared up front. Live runs
        // default to lazy prep (`NXR_LAZY_PREP=off` restores eager).
        let use_lazy = lazy_prep_enabled() && !dry_run && cache.is_none();

        let (prepared_nodes, retained_snapshot) = if use_lazy {
            if WorkspaceSnapshot::plan_requires_project_trust(
                &document,
                &plan,
                &request.environment_policy,
                request.context_override.as_deref(),
            )? {
                trust::enforce_for_execution(
                    &snapshot.flake.display,
                    snapshot.flake.local_root.as_deref(),
                    &snapshot.flake.nix_ref,
                )?;
            }
            (BTreeMap::new(), Some(snapshot.clone()))
        } else {
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
                request.context_override.as_deref(),
            )?;

            if !dry_run && requires_project_trust(&prepared_nodes) {
                trust::enforce_for_execution(
                    &snapshot.flake.display,
                    snapshot.flake.local_root.as_deref(),
                    &snapshot.flake.nix_ref,
                )?;
            }
            (prepared_nodes, None)
        };

        let bundle = PreparedTaskGeneration {
            document,
            plan,
            prepared_nodes,
            canonical_roots,
            snapshot: retained_snapshot,
        };
        if let Some(cache) = cache {
            *cache = Some(bundle.clone());
        }
        bundle
    };

    let PreparedTaskGeneration {
        document,
        plan,
        prepared_nodes,
        canonical_roots,
        snapshot: lazy_snapshot,
    } = prepared_bundle;

    let failure_policy = if request.keep_going {
        FailurePolicy::KeepGoing
    } else {
        FailurePolicy::FailFast
    };

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
        if !request.reports.is_empty() {
            write_all_reports(&request.reports, &[], None)?;
        }
        return dry_run_execute(&prepared_nodes, &plan, &waves, request, json, runner);
    }

    let mut preparer = if let Some(ref snapshot) = lazy_snapshot {
        TaskNodePreparer::new(
            snapshot,
            &document,
            &canonical_roots,
            request.args,
            request.root,
            request.cwd,
            request.shell,
            request.shell_mode,
            &request.environment_policy,
            request.nix_flags,
            request.context_override.as_deref(),
        )?
    } else {
        TaskNodePreparer::from_prepared(prepared_nodes)
    };

    if pipe_stdio {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        let inner = build_task_event_sink(
            effective_output,
            request.events_format,
            &mut stdout,
            &mut stderr,
        );
        let mut sink = wrap_report_sink(inner, &request.reports);
        sink.emit(Event::plan_created(
            plan.root.clone(),
            if plan.roots.is_empty() {
                None
            } else {
                Some(plan.roots.clone())
            },
            plan.nodes.len(),
        ));
        let result = run_plan(request, &plan, &mut preparer, &mut sink, runner, control);
        report_sink_error(&sink)?;
        result
    } else {
        // Inherit stdio for interactivity / --output raw: do not hold stdout/stderr locks.
        let inner = nxr_task::NullSink;
        let mut sink = wrap_report_sink(inner, &request.reports);
        let result = run_plan(request, &plan, &mut preparer, &mut sink, runner, control);
        report_sink_error(&sink)?;
        result
    }
}

fn wrap_report_sink<S: EventSink>(
    inner: S,
    reports: &ReportPaths,
) -> RunEventDecorator<ReportCollector<S>> {
    let inner = if reports.is_empty() {
        ReportCollector::new(inner, ReportPaths::default())
    } else {
        ReportCollector::new(inner, reports.clone())
    };
    RunEventDecorator::new(inner)
}

fn report_sink_error<S>(sink: &RunEventDecorator<ReportCollector<S>>) -> Result<(), TaskError> {
    if let Some(message) = sink.inner().write_error() {
        return Err(TaskError::Report(ReportWriteError::Serialize(
            message.to_owned(),
        )));
    }
    Ok(())
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
        let cache_explain = explain_workspace_cache(prepared);
        let tier_label = match cache_explain.tier {
            nxr_core::ActionTier::DerivationBacked => "derivation-backed",
            nxr_core::ActionTier::WorkspaceAction => "workspace-action",
        };
        let lookup_label = match &cache_explain.lookup {
            nxr_core::cas::CacheLookupExplain::Hit => "hit",
            nxr_core::cas::CacheLookupExplain::Miss { reason } => reason.as_str(),
            nxr_core::cas::CacheLookupExplain::Skipped { reason } => reason.as_str(),
        };
        writeln!(
            stdout,
            "# cache {}: tier={} enabled={} key={} lookup={}",
            task_id,
            tier_label,
            cache_explain.cache_enabled,
            cache_explain.action_key.as_deref().unwrap_or("-"),
            lookup_label
        )
        .map_err(TaskError::Io)?;
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
    preparer: &mut TaskNodePreparer<'_>,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
    control: &mut dyn FnMut() -> io::Result<RunControl>,
) -> Result<i32, TaskError> {
    let mut scheduler = Scheduler::new(plan, request.jobs)?;
    let mut supervisor = Supervisor::new();
    let mut pipe_io = PipeMultiplexer::new();
    let mut deadlines = DeadlineQueue::new();
    let mut node_compact_ids: BTreeMap<String, u32> = BTreeMap::new();
    let interrupts = InterruptFlags::install().map_err(TaskError::Supervision)?;

    let mut first_failure: Option<i32> = None;
    let mut interrupted = false;
    let mut restarted = false;
    let mut started_at: BTreeMap<String, std::time::Instant> = BTreeMap::new();

    let (user_config, secret_bindings) = load_runtime_secret_config()?;
    let project_id = project_identity(std::path::Path::new(preparer.flake_root().as_str()));
    let mut node_secret_guards: BTreeMap<String, SpawnSecrets> = BTreeMap::new();

    let mut to_start = scheduler.schedule_ready();
    loop {
        if let Some(codes) = supervisor
            .handle_interrupt(&interrupts, SHUTDOWN_GRACE)
            .map_err(TaskError::Supervision)?
        {
            interrupted = true;
            drain_pipe_chunks(
                &mut pipe_io,
                sink,
                SHUTDOWN_GRACE,
                PipeDrainMode::ForcedShutdown,
            )?;
            for (id, code) in codes {
                sink.emit(Event::NodeExited {
                    node: id.clone(),
                    code: Some(code),
                    status: Some(nxr_task::NodeOutcome::Cancelled),
                    duration_ms: started_at.remove(&id).map(duration_ms_since),
                    started_at: None,
                    finished_at: None,
                    reason: Some("interrupted".to_owned()),
                    seq: None,
                });
                if let Some(compact) = node_compact_ids.remove(&id) {
                    if pipe_io.has_pipes(compact) {
                        pipe_io.remove_node(compact);
                    }
                    deadlines.cancel(compact);
                }
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
                drain_pipe_chunks(
                    &mut pipe_io,
                    sink,
                    SHUTDOWN_GRACE,
                    PipeDrainMode::ForcedShutdown,
                )?;
                for (stopped_id, stopped_code) in shut {
                    sink.emit(Event::NodeExited {
                        node: stopped_id.clone(),
                        code: Some(stopped_code),
                        status: Some(nxr_task::NodeOutcome::Cancelled),
                        duration_ms: started_at.remove(&stopped_id).map(duration_ms_since),
                        started_at: None,
                        finished_at: None,
                        reason: Some(match signal {
                            RunControl::Restart => "watch_restart".to_owned(),
                            _ => "stopped".to_owned(),
                        }),
                        seq: None,
                    });
                    if let Some(compact) = node_compact_ids.remove(&stopped_id) {
                        if pipe_io.has_pipes(compact) {
                            pipe_io.remove_node(compact);
                        }
                        deadlines.cancel(compact);
                    }
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

        // Enforce per-task timeouts before starting more work.
        let now = std::time::Instant::now();
        let timed_out = deadlines.pop_expired(now);
        for compact in timed_out {
            let id = pipe_io.node_label(compact).to_owned();
            // A peer timeout under fail-fast may have already shut this node down.
            if !started_at.contains_key(&id) {
                continue;
            }
            let grace = preparer
                .prepared()
                .get(&id)
                .and_then(|node| node.termination_grace)
                .unwrap_or(SHUTDOWN_GRACE);
            let Some(code) = supervisor
                .shutdown_one(&id, grace)
                .map_err(TaskError::Supervision)?
            else {
                started_at.remove(&id);
                continue;
            };
            sink.emit(Event::NodeExited {
                node: id.clone(),
                code: Some(code),
                status: Some(nxr_task::NodeOutcome::TimedOut),
                duration_ms: started_at.remove(&id).map(duration_ms_since),
                started_at: None,
                finished_at: None,
                reason: Some("timeout".to_owned()),
                seq: None,
            });
            deadlines.cancel(compact);
            if first_failure.is_none() {
                first_failure = Some(code);
            }
            to_start.extend(scheduler.complete(&id, code)?);
            if scheduler.failure_policy() == FailurePolicy::FailFast && !supervisor.is_empty() {
                let shut = supervisor
                    .shutdown_all(SHUTDOWN_GRACE)
                    .map_err(TaskError::Supervision)?;
                for (stopped_id, stopped_code) in shut {
                    sink.emit(Event::NodeExited {
                        node: stopped_id.clone(),
                        code: Some(stopped_code),
                        status: Some(nxr_task::NodeOutcome::Cancelled),
                        duration_ms: started_at.remove(&stopped_id).map(duration_ms_since),
                        started_at: None,
                        finished_at: None,
                        reason: Some("fail_fast".to_owned()),
                        seq: None,
                    });
                    let _ = scheduler.on_exit(&stopped_id, stopped_code);
                }
                // Do not re-process remaining timed-out peers that fail-fast just cancelled.
                break;
            }
        }

        // Phase 3: prepare CAS inputs for resource-ready nodes about to start.
        // SpawnPlan is deferred until after (or overlapped with) CAS lookup when
        // pipelining is enabled (ADR-0159).
        if preparer.pipeline_enabled() {
            preparer
                .ensure_cas_inputs_many(&to_start)
                .map_err(TaskError::Prepare)?;
        } else {
            preparer
                .ensure_prepared_many(&to_start)
                .map_err(TaskError::Prepare)?;
        }
        // Phase 4 (optional): speculate successors only under keep-going so
        // fail-fast / upstream failure still avoid never-run prepares.
        if request.keep_going {
            preparer
                .speculate_successors(plan, &to_start, request.jobs)
                .map_err(TaskError::Prepare)?;
        }

        let ready: Vec<String> = std::mem::take(&mut to_start);
        let mut spawn_queue = Vec::new();
        let mut inflight_spawn_plans = Vec::new();
        for node_id in ready {
            preparer
                .ensure_stage(&node_id, NodePrepStage::CasInputs)
                .map_err(TaskError::Prepare)?;

            // Overlap SpawnPlan with CAS lookup when pipelining; cancel on hit.
            let ticket = if preparer.pipeline_enabled() {
                Some(
                    preparer
                        .start_spawn_plan(&node_id)
                        .map_err(TaskError::Prepare)?,
                )
            } else {
                None
            };

            let prepared = preparer
                .prepared()
                .get(&node_id)
                .expect("ensure_stage just prepared this node");
            if try_workspace_cache_restore(prepared, &prepared.flake_root)
                .map_err(TaskError::Supervision)?
                .is_some()
            {
                if let Some(ticket) = ticket {
                    preparer.cancel_spawn_plan(ticket);
                }
                runner
                    .verbose(format!("cache hit for task {node_id}; skipping spawn"))
                    .map_err(TaskError::Io)?;
                sink.emit(Event::node_started(node_id.clone()));
                sink.emit(Event::NodeExited {
                    node: node_id.clone(),
                    code: Some(exit::SUCCESS),
                    status: Some(nxr_task::NodeOutcome::Succeeded),
                    duration_ms: Some(0),
                    started_at: None,
                    finished_at: None,
                    reason: Some("cache_hit".to_owned()),
                    seq: None,
                });
                to_start.extend(scheduler.complete(&node_id, exit::SUCCESS)?);
                continue;
            }

            if let Some(ticket) = ticket {
                inflight_spawn_plans.push(ticket);
            } else {
                preparer
                    .ensure_stage(&node_id, NodePrepStage::SpawnPlan)
                    .map_err(TaskError::Prepare)?;
            }
            spawn_queue.push(node_id);
        }

        // Join overlapped SpawnPlan work before spawning (miss path).
        for ticket in inflight_spawn_plans {
            let node_id = ticket.task_id().to_owned();
            preparer
                .join_spawn_plan(ticket)
                .map_err(TaskError::Prepare)?;
            // join may have cancelled; ensure SpawnPlan for miss-path nodes.
            if preparer
                .prepared()
                .get(&node_id)
                .is_some_and(|n| n.prep_stage < NodePrepStage::SpawnPlan)
            {
                preparer
                    .ensure_stage(&node_id, NodePrepStage::SpawnPlan)
                    .map_err(TaskError::Prepare)?;
            }
        }

        for node_id in spawn_queue {
            let pipe_stdio = node_uses_piped_stdio(
                node_is_interactive(plan, &node_id),
                request.jobs,
                request.output_mode,
                request.events_format,
            );
            let compact = spawn_node(
                preparer,
                &node_id,
                pipe_stdio,
                &mut supervisor,
                &mut pipe_io,
                sink,
                runner,
                &user_config,
                &secret_bindings,
                &project_id,
                &mut node_secret_guards,
            )?;
            started_at.insert(node_id.clone(), std::time::Instant::now());
            node_compact_ids.insert(node_id.clone(), compact);
            if let Some(timeout) = preparer
                .prepared()
                .get(&node_id)
                .and_then(|node| node.timeout)
            {
                deadlines.insert(compact, started_at[&node_id] + timeout);
            }
        }

        let poll_timeout = deadlines
            .time_until_next(std::time::Instant::now())
            .map_or(POLL_INTERVAL, |remaining| remaining.min(POLL_INTERVAL));
        drain_pipe_chunks(&mut pipe_io, sink, poll_timeout, PipeDrainMode::Normal)?;
        cleanup_closed_pipes(&pipe_io, &mut deadlines, &mut node_compact_ids);

        if let Some((id, code)) = supervisor.try_wait_any().map_err(TaskError::Supervision)? {
            let status = if code == exit::SUCCESS {
                nxr_task::NodeOutcome::Succeeded
            } else {
                nxr_task::NodeOutcome::Failed
            };
            sink.emit(Event::NodeExited {
                node: id.clone(),
                code: Some(code),
                status: Some(status),
                duration_ms: started_at.remove(&id).map(duration_ms_since),
                started_at: None,
                finished_at: None,
                reason: None,
                seq: None,
            });
            if let Some(&compact) = node_compact_ids.get(&id) {
                deadlines.cancel(compact);
            }
            node_secret_guards.remove(&id);

            if code == exit::SUCCESS
                && let Some(prepared) = preparer.prepared().get(&id)
            {
                save_workspace_cache(prepared, &prepared.flake_root)
                    .map_err(TaskError::Supervision)?;
            }

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
                        status: Some(nxr_task::NodeOutcome::Cancelled),
                        duration_ms: started_at.remove(&stopped_id).map(duration_ms_since),
                        started_at: None,
                        finished_at: None,
                        reason: Some("fail_fast".to_owned()),
                        seq: None,
                    });
                    let _ = scheduler.on_exit(&stopped_id, stopped_code);
                }
            }
            continue;
        }

        if scheduler.is_finished() && supervisor.is_empty() && node_compact_ids.is_empty() {
            break;
        }

        let idle_timeout = deadlines
            .time_until_next(std::time::Instant::now())
            .map_or(POLL_INTERVAL, |remaining| remaining.min(POLL_INTERVAL));
        drain_pipe_chunks(&mut pipe_io, sink, idle_timeout, PipeDrainMode::Normal)?;
        cleanup_closed_pipes(&pipe_io, &mut deadlines, &mut node_compact_ids);
    }

    // Flush any trailing pipe chunks after the last exit.
    drain_pipe_chunks(&mut pipe_io, sink, Duration::ZERO, PipeDrainMode::Normal)?;
    cleanup_closed_pipes(&pipe_io, &mut deadlines, &mut node_compact_ids);

    let outcome = scheduler.outcome();
    // Emit exactly one terminal event for nodes that never started (skipped /
    // fail-fast cancelled). Running nodes already emitted NodeExited above.
    for id in &outcome.skipped_nodes {
        sink.emit(Event::NodeExited {
            node: id.clone(),
            code: None,
            status: Some(nxr_task::NodeOutcome::Skipped),
            duration_ms: None,
            started_at: None,
            finished_at: None,
            reason: Some("dependency_failed".to_owned()),
            seq: None,
        });
    }
    for id in &outcome.cancelled_nodes {
        sink.emit(Event::NodeExited {
            node: id.clone(),
            code: None,
            status: Some(nxr_task::NodeOutcome::Cancelled),
            duration_ms: None,
            started_at: None,
            finished_at: None,
            reason: Some("fail_fast".to_owned()),
            seq: None,
        });
    }

    let success = !interrupted && !restarted && outcome.success;
    let run_status = if interrupted {
        Some(nxr_task::RunOutcome::Cancelled)
    } else if success {
        Some(nxr_task::RunOutcome::Succeeded)
    } else {
        Some(nxr_task::RunOutcome::Failed)
    };
    sink.emit(Event::RunCompleted {
        success,
        run_id: None,
        status: run_status,
        duration_ms: None,
        started_at: None,
        finished_at: None,
    });

    runner
        .verbose(format!(
            "prepared {} task node(s) this run",
            preparer.prepare_count()
        ))
        .map_err(TaskError::Io)?;

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

fn duration_ms_since(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn requires_project_trust(prepared_nodes: &BTreeMap<String, PreparedTaskNode>) -> bool {
    prepared_nodes
        .values()
        .any(|node| node.confirm || !node.plan.secrets.is_empty())
}

fn spawn_node(
    preparer: &TaskNodePreparer<'_>,
    node_id: &str,
    pipe_stdio: bool,
    supervisor: &mut Supervisor,
    pipe_io: &mut PipeMultiplexer,
    sink: &mut dyn EventSink,
    runner: RunnerOutput,
    user_config: &nxr_core::config::UserConfig,
    secret_bindings: &nxr_core::config::SecretBindings,
    project_id: &str,
    node_secret_guards: &mut BTreeMap<String, SpawnSecrets>,
) -> Result<u32, TaskError> {
    let prepared = preparer
        .prepared()
        .get(node_id)
        .expect("scheduler only starts ids prepared before run");
    debug_assert!(
        prepared.prep_stage >= NodePrepStage::SpawnPlan,
        "spawn requires SpawnPlan stage"
    );
    if prepared.confirm {
        let context_name = prepared.context_name.as_deref().unwrap_or("unknown");
        enforce_context_confirm(context_name, node_id, true).map_err(TaskError::Context)?;
    }
    runner
        .verbose(format!(
            "running task {node_id} via app {}",
            prepared.plan.target
        ))
        .map_err(TaskError::Io)?;

    sink.emit(Event::node_started(node_id.to_owned()));

    let spawn = crate::commands::store_exe::resolve_app_spawn(
        &prepared.plan,
        &prepared.program,
        Some(prepared.flake_root.as_path()),
        &OptionalNixFlags::default(),
        "",
        Some(prepared.cwd.as_std_path()),
    );
    let program = spawn.program.as_std_path();
    let args = &spawn.arguments;
    let cwd = Some(prepared.cwd.as_std_path());
    let env = &prepared.environment;
    let (env_overrides, stdin_payload, spawn_secrets) =
        build_spawn_env_overrides(&prepared.plan, user_config, secret_bindings, project_id)?;
    if stdin_payload.is_some() && pipe_stdio {
        return Err(TaskError::Context(ContextError::UnsupportedDelivery {
            slot: "stdin".to_owned(),
            reference: "stdin".to_owned(),
            delivery: SecretDelivery::Stdin,
        }));
    }
    node_secret_guards.insert(node_id.to_owned(), spawn_secrets);
    let compact = pipe_io.intern_node(node_id);

    if pipe_stdio {
        let (_pgid, stdout, stderr) = supervisor
            .spawn_piped(
                node_id.to_owned(),
                program,
                args,
                cwd,
                env,
                env_overrides.as_ref(),
            )
            .map_err(TaskError::Supervision)?;
        pipe_io
            .register_stdout(compact, stdout)
            .map_err(TaskError::Supervision)?;
        pipe_io
            .register_stderr(compact, stderr)
            .map_err(TaskError::Supervision)?;
    } else {
        supervisor
            .spawn(
                node_id.to_owned(),
                program,
                args,
                cwd,
                env,
                env_overrides.as_ref(),
                stdin_payload,
            )
            .map_err(TaskError::Supervision)?;
    }

    Ok(compact)
}

fn build_spawn_env_overrides(
    plan: &nxr_core::Plan,
    user_config: &nxr_core::config::UserConfig,
    secret_bindings: &nxr_core::config::SecretBindings,
    project_id: &str,
) -> Result<
    (
        Option<std::collections::BTreeMap<String, String>>,
        Option<Vec<u8>>,
        SpawnSecrets,
    ),
    TaskError,
> {
    if plan.secrets.is_empty() {
        let merged = merge_spawn_env_overrides(&plan.context_env_set, &BTreeMap::new());
        let empty = SpawnSecrets::empty();
        return Ok((
            if merged.is_empty() {
                None
            } else {
                Some(merged)
            },
            None,
            empty,
        ));
    }
    let entries = plan_secret_entries_from_core(&plan.secrets);
    let spawn_secrets = prepare_spawn_secrets(&entries, project_id, user_config, secret_bindings)?;
    let merged = merge_spawn_env_overrides(&plan.context_env_set, &spawn_secrets.env_overrides);
    let env_overrides = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };
    Ok((
        env_overrides,
        spawn_secrets.stdin_payload.clone(),
        spawn_secrets,
    ))
}

fn plan_secret_entries_from_core(secrets: &[PlanSecretRef]) -> Vec<PlanSecretEntry> {
    secrets
        .iter()
        .map(|secret| PlanSecretEntry {
            name: secret.name.clone(),
            reference: secret.reference.clone(),
            delivery: parse_plan_secret_delivery(&secret.delivery),
            provider: parse_plan_secret_provider(&secret.provider),
            value: PlanSecretValuePlaceholder::RUNTIME,
        })
        .collect()
}

fn parse_plan_secret_delivery(label: &str) -> SecretDelivery {
    match label {
        "file" => SecretDelivery::File,
        "stdin" => SecretDelivery::Stdin,
        _ => SecretDelivery::Env,
    }
}

fn parse_plan_secret_provider(label: &str) -> SecretProvider {
    match label {
        "file" => SecretProvider::File,
        "sops" => SecretProvider::Sops,
        "sops-nix" => SecretProvider::SopsNix,
        _ => SecretProvider::Env,
    }
}

fn cleanup_closed_pipes(
    pipe_io: &PipeMultiplexer,
    deadlines: &mut DeadlineQueue,
    node_compact_ids: &mut BTreeMap<String, u32>,
) {
    node_compact_ids.retain(|_node_id, compact| {
        if pipe_io.has_pipes(*compact) {
            true
        } else {
            deadlines.cancel(*compact);
            false
        }
    });
}

fn drain_pipe_chunks(
    pipe_io: &mut PipeMultiplexer,
    sink: &mut dyn EventSink,
    timeout: Duration,
    mode: PipeDrainMode,
) -> Result<(), TaskError> {
    let mut pending = Vec::new();
    match pipe_io.poll(timeout, |chunk| pending.push(chunk)) {
        Ok(()) => {}
        // Forced shutdown tears down poll registrations while fds may still be
        // readable; mio can surface EINTR without losing already-buffered chunks.
        Err(error)
            if mode == PipeDrainMode::ForcedShutdown
                && error.kind() == io::ErrorKind::Interrupted => {}
        Err(error) => return Err(TaskError::Supervision(error)),
    }
    for chunk in pending {
        let node = pipe_io.node_label(chunk.node).to_owned();
        let payload = OutputPayload::from_bytes(chunk.bytes);
        match chunk.stream {
            PipeStream::Stdout => sink.emit(Event::StdoutChunk { node, payload }),
            PipeStream::Stderr => sink.emit(Event::StderrChunk { node, payload }),
        }
    }
    Ok(())
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
    use crate::reports::ReportPaths;
    use nxr_core::EnvironmentPolicy;
    use nxr_core::diagnostics::exit;
    use nxr_task::{
        FailurePolicy, PlanError, TaskDefinition, build_execution_plan, build_execution_plan_roots,
    };
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
            tasks: task_names,
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
            reports: ReportPaths::default(),
            nix_flags: &nix_flags,
            context_override: None,
            refresh_discovery: false,
        };
        assert!(plan_uses_piped_stdio(&plan, &request));
    }

    /// Mirrors the task run loop when multiple ready nodes are cache hits in one
    /// batch: each `complete` must extend `to_start`, not overwrite it.
    #[test]
    fn cache_hit_ready_batch_accumulates_unlocked_nodes() {
        use nxr_task::Scheduler;

        let mut tasks = BTreeMap::new();
        tasks.insert("root1".to_owned(), TaskDefinition::new("root1"));
        tasks.insert("root2".to_owned(), TaskDefinition::new("root2"));
        let mut child1 = TaskDefinition::new("child1");
        child1.depends_on = vec!["root1".to_owned()];
        tasks.insert("child1".to_owned(), child1);
        let mut child2 = TaskDefinition::new("child2");
        child2.depends_on = vec!["root2".to_owned()];
        tasks.insert("child2".to_owned(), child2);

        let plan = build_execution_plan_roots(
            &tasks,
            &["child1", "child2"],
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let mut scheduler = Scheduler::new(&plan, 2).expect("scheduler");
        let ready = scheduler.schedule_ready();
        assert_eq!(ready, vec!["root1".to_owned(), "root2".to_owned()]);

        let mut to_start = Vec::new();
        for node_id in ready {
            to_start.extend(
                scheduler
                    .complete(&node_id, exit::SUCCESS)
                    .expect("cache-hit complete"),
            );
        }

        let mut unlocked: Vec<_> = to_start;
        unlocked.sort();
        assert_eq!(unlocked, vec!["child1".to_owned(), "child2".to_owned()]);
    }
}
