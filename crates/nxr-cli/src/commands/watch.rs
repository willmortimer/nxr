//! `nxr watch` — flake-root filesystem watch with kill+rerun generations.

use std::io::{self, Write};
use std::time::Duration;

use camino::Utf8PathBuf;
use nxr_completion::cache::{WorkspaceDiscovery, cached_workspace};
use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{
    AppNotFoundError, NixError, OptionalNixFlags, TaskDiscoveryError, resolve_app_by_name,
};
use nxr_process::{InterruptFlags, Supervisor, spawn_in};
use nxr_task::{PlanError, resolve_task_name};
use nxr_watch::{
    ChangeClass, Generation, MetadataInputRegistry, PathFilterError, PathFilters, PrewarmCasHandle,
    PrewarmContext, WatchConfig, WatchError, WatchIncrementalSnapshot, WatchPoll,
    WatchSemanticCoalescer, WatchSession, classify_pending_changes,
};

use crate::commands::common::{
    AppRequest, PrepareError, PreparedPlan, TaskNodePreparer, WorkspaceSnapshot, WorkspaceState,
    current_invocation_directory, prepare_app_plan_in_state,
};
use crate::commands::store_exe::resolve_app_spawn_with_prewarm;
use crate::commands::task::{self, PreparedTaskGeneration, TaskError, TaskRequest, plan_exit_code};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::output_task::{EventsFormat, TaskOutputMode};
use crate::reports::ReportPaths;
use crate::runner_output::RunnerOutput;

/// Default debounce when the CLI omits `--debounce`.
pub const DEFAULT_DEBOUNCE_MS: u64 = 300;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Watch-specific CLI options shared by `watch`, `run --watch`, and `task --watch`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchOptions {
    pub debounce: Duration,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub clear: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            include: Vec::new(),
            exclude: Vec::new(),
            clear: false,
        }
    }
}

impl WatchOptions {
    /// Build options from `nxr watch` CLI flags.
    #[must_use]
    pub fn from_cli(debounce_ms: u64, include: &[String], exclude: &[String], clear: bool) -> Self {
        Self {
            debounce: Duration::from_millis(debounce_ms),
            include: include.to_vec(),
            exclude: exclude.to_vec(),
            clear,
        }
    }
}

/// Task-scheduler options preserved across watch generations (`task --watch`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskWatchSettings {
    /// One or more task roots (union DAG).
    pub tasks: Vec<String>,
    pub jobs: usize,
    pub keep_going: bool,
    pub output_mode: Option<TaskOutputMode>,
    pub events_format: Option<EventsFormat>,
    pub reports: ReportPaths,
    pub param_sets: std::collections::BTreeMap<String, String>,
    pub log_dir: Option<std::path::PathBuf>,
}

/// Inputs for watch mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    /// App or single task name when [`Self::task_settings`] is `None`.
    pub name: &'a str,
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_mode: crate::shell_mode::ShellMode,
    pub environment_policy: EnvironmentPolicy,
    pub options: WatchOptions,
    /// Global `--output` (honored for task generations).
    pub output_mode: Option<TaskOutputMode>,
    /// Global `--events` (honored for task generations).
    pub events_format: Option<EventsFormat>,
    /// When set, watch runs the normal task scheduler (multi-root, `-j`, output).
    pub task_settings: Option<TaskWatchSettings>,
    /// Resolve as an app without loading tasks (`nxr run --watch`, or `app:` prefix).
    pub force_app: bool,
    pub nix_flags: &'a nxr_nix::OptionalNixFlags,
}

/// Errors while watching and re-running a target.
#[derive(Debug, thiserror::Error)]
pub enum WatchCommandError {
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
    Task(#[from] TaskError),
    #[error(transparent)]
    NotFound(#[from] AppNotFoundError),
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    Filter(#[from] PathFilterError),
    #[error("nxr watch requires a local flake path (got a remote reference)")]
    RemoteFlake,
    #[error("failed to supervise watch generation: {0}")]
    Supervision(#[source] io::Error),
    #[error("failed to write runner diagnostics: {0}")]
    Io(#[source] io::Error),
}

impl WatchCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Discovery(error) => error.exit_code(),
            Self::Plan(error) => plan_exit_code(error),
            Self::Task(error) => error.exit_code(),
            Self::NotFound(error) => error.exit_code(),
            Self::Watch(_) | Self::Filter(_) | Self::RemoteFlake => exit::DISCOVERY,
            Self::Supervision(_) | Self::Io(_) => exit::PROCESS_SUPERVISION,
        }
    }
}

#[derive(Clone, Debug)]
enum WatchTarget {
    App { name: String },
    Task { name: String },
}

enum GenerationOutcome {
    /// Target finished; wait for the next filesystem change.
    Idle,
    /// Filesystem change — start a new generation immediately.
    Restart,
    /// Ctrl-C / SIGTERM — stop watching.
    Stopped { code: i32 },
}

#[derive(Default)]
struct WatchCaches {
    app_plan: Option<PreparedPlan>,
    task_plan: Option<PreparedTaskGeneration>,
    incremental: Option<WatchIncrementalSnapshot>,
    coalesce: WatchSemanticCoalescer,
}

/// Resolve `name` as a task (preferred) or app, then watch the flake root.
///
/// Task targets use the normal [`task::execute`] pipeline each generation
/// (`WorkspaceSnapshot` → `ExecutionPlan` → `PreparedTaskNode` → `Scheduler`),
/// preserving `-j`, `--keep-going`, working directories, output/events, and exit
/// codes. Metadata inputs (`.nix`, `flake.lock`, projects, `discoveryInputs`)
/// invalidate the snapshot; ordinary source edits reuse the prepared plan.
///
/// # Errors
///
/// Returns [`WatchCommandError`] on resolution, watcher, or supervision failures.
///
/// On interrupt, returns [`exit::INTERRUPTED`] after cleaning up the current
/// generation.
pub fn run(request: &WatchRequest<'_>, runner: RunnerOutput) -> Result<i32, WatchCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let watch_root = flake
        .local_root
        .clone()
        .ok_or(WatchCommandError::RemoteFlake)?;

    let mut workspace =
        WorkspaceState::new(request.flake_arg, request.nix_override, request.nix_flags);
    let target = resolve_target(request, &mut workspace)?;

    let filters = PathFilters::new(&request.options.include, &request.options.exclude)?;
    let mut session = WatchSession::start(&WatchConfig {
        root: watch_root.clone(),
        debounce: request.options.debounce,
        filters: filters.clone(),
    })?;
    let interrupts = InterruptFlags::install().map_err(WatchCommandError::Supervision)?;
    let mut generation = Generation::new();
    let mut metadata_registry = MetadataInputRegistry::new();
    let mut caches = WatchCaches {
        incremental: WatchIncrementalSnapshot::enabled()
            .then(|| WatchIncrementalSnapshot::new(watch_root.clone())),
        ..WatchCaches::default()
    };

    runner
        .info(format!(
            "watching {} for changes (debounce {}ms); Ctrl-C to stop",
            watch_root,
            request.options.debounce.as_millis()
        ))
        .map_err(WatchCommandError::Io)?;

    loop {
        let generation_id = generation.bump();
        if request.options.clear && generation_id > 1 {
            clear_terminal().map_err(WatchCommandError::Io)?;
        }

        let pending_changes = session.take_pending_changes();
        let pending_changes = coalesce_pending_paths(&watch_root, &mut caches, pending_changes);
        if generation_id > 1 && pending_changes.is_empty() {
            runner
                .verbose("watch coalesce: spurious restart suppressed")
                .map_err(WatchCommandError::Io)?;
            loop {
                if interrupts.take_pending() {
                    return Ok(exit::INTERRUPTED);
                }
                match session.poll_restart(Duration::from_millis(100))? {
                    WatchPoll::Restart => break,
                    WatchPoll::Timeout => {}
                }
            }
            continue;
        }

        let invalidate_snapshot = apply_restart_classification(
            &watch_root,
            &filters,
            &pending_changes,
            &mut metadata_registry,
            &mut workspace,
            &mut caches,
            runner,
        )?;
        if invalidate_snapshot || generation_id == 1 {
            refresh_metadata_registry(&target, &mut workspace, &mut metadata_registry)?;
            seed_incremental_graph(&target, &mut workspace, &mut caches)?;
        }

        runner
            .verbose(format!("watch generation {generation_id}"))
            .map_err(WatchCommandError::Io)?;

        match run_generation(
            request,
            &target,
            &mut workspace,
            &mut session,
            &interrupts,
            &mut caches,
            invalidate_snapshot,
            runner,
        )? {
            GenerationOutcome::Idle => loop {
                if interrupts.take_pending() {
                    return Ok(exit::INTERRUPTED);
                }
                match session.poll_restart(Duration::from_millis(100))? {
                    WatchPoll::Restart => break,
                    WatchPoll::Timeout => {}
                }
            },
            GenerationOutcome::Restart => {}
            GenerationOutcome::Stopped { code } => return Ok(code),
        }
    }
}

fn resolve_target(
    request: &WatchRequest<'_>,
    workspace: &mut WorkspaceState<'_>,
) -> Result<WatchTarget, WatchCommandError> {
    if request.task_settings.is_some() {
        return Ok(WatchTarget::Task {
            name: request.name.to_owned(),
        });
    }

    match parse_watch_name(request.name) {
        WatchNameKind::App { name } => resolve_app_target(workspace, name),
        WatchNameKind::Task { name } => Ok(WatchTarget::Task {
            name: name.to_owned(),
        }),
        WatchNameKind::Unprefixed { name } => {
            if request.force_app {
                return resolve_app_target(workspace, name);
            }
            resolve_unprefixed_target(workspace, name)
        }
    }
}

fn resolve_app_target(
    workspace: &mut WorkspaceState<'_>,
    name: &str,
) -> Result<WatchTarget, WatchCommandError> {
    // Apps-only snapshot avoids task evaluation when the caller disambiguates.
    let snapshot = workspace.snapshot(false)?;
    resolve_app_target_from_snapshot(snapshot, name)
}

fn resolve_app_target_from_snapshot(
    snapshot: &WorkspaceSnapshot,
    name: &str,
) -> Result<WatchTarget, WatchCommandError> {
    let apps: Vec<_> = snapshot.apps.values().cloned().collect();
    let app = resolve_app_by_name(&apps, name)?;
    Ok(WatchTarget::App {
        name: app.name.clone(),
    })
}

/// Resolve an unprefixed watch name with task-wins when both exist.
///
/// Uses the discovery cache task-name set when present so app-only targets (for
/// example `hello` on `fixtures/basic-apps`) avoid task `eval` on first generation.
/// When task metadata is unknown (`nxr` may exist), loads tasks before deciding.
fn resolve_unprefixed_target(
    workspace: &mut WorkspaceState<'_>,
    name: &str,
) -> Result<WatchTarget, WatchCommandError> {
    if let Some(cached) = cached_discovery(workspace)?
        && let Some(document) = cached.tasks.as_ref()
    {
        if resolve_task_name(document, name).is_ok() {
            return Ok(WatchTarget::Task {
                name: name.to_owned(),
            });
        }
        return resolve_app_target(workspace, name);
    }

    let snapshot = workspace.snapshot(false)?;
    if let Some(document) = snapshot.tasks.as_ref() {
        if resolve_task_name(document, name).is_ok() {
            return Ok(WatchTarget::Task {
                name: name.to_owned(),
            });
        }
        return resolve_app_target_from_snapshot(snapshot, name);
    }

    resolve_unprefixed_after_tasks_loaded(workspace, name)
}

fn cached_discovery(
    workspace: &mut WorkspaceState<'_>,
) -> Result<Option<WorkspaceDiscovery>, WatchCommandError> {
    let context = workspace
        .discovery_context()
        .map_err(WatchCommandError::Prepare)?;
    Ok(cached_workspace(&context))
}

fn resolve_unprefixed_after_tasks_loaded(
    workspace: &mut WorkspaceState<'_>,
    name: &str,
) -> Result<WatchTarget, WatchCommandError> {
    let snapshot = workspace.snapshot(true)?;
    let document = snapshot
        .tasks
        .as_ref()
        .expect("load_tasks=true always populates tasks");
    if resolve_task_name(document, name).is_ok() {
        Ok(WatchTarget::Task {
            name: name.to_owned(),
        })
    } else {
        resolve_app_target_from_snapshot(snapshot, name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchNameKind<'a> {
    App { name: &'a str },
    Task { name: &'a str },
    Unprefixed { name: &'a str },
}

fn parse_watch_name(raw: &str) -> WatchNameKind<'_> {
    if let Some(name) = raw.strip_prefix("app:") {
        return WatchNameKind::App { name };
    }
    if let Some(name) = raw.strip_prefix("task:") {
        return WatchNameKind::Task { name };
    }
    WatchNameKind::Unprefixed { name: raw }
}

fn refresh_metadata_registry(
    target: &WatchTarget,
    workspace: &mut WorkspaceState<'_>,
    registry: &mut MetadataInputRegistry,
) -> Result<(), WatchCommandError> {
    match target {
        WatchTarget::App { .. } => {
            // App watch does not need task discoveryInputs; flake.nix / lock /
            // .nix tree coverage already comes from the default metadata set.
            registry.set_discovery_inputs(Vec::new());
        }
        WatchTarget::Task { .. } => {
            let snapshot = workspace.snapshot(true)?;
            if let Some(document) = snapshot.tasks.as_ref() {
                registry.set_discovery_inputs(document.discovery_inputs.clone());
            }
        }
    }
    Ok(())
}

fn seed_incremental_graph(
    target: &WatchTarget,
    workspace: &mut WorkspaceState<'_>,
    caches: &mut WatchCaches,
) -> Result<(), WatchCommandError> {
    let Some(incremental) = caches.incremental.as_mut() else {
        return Ok(());
    };
    if !matches!(target, WatchTarget::Task { .. }) {
        return Ok(());
    }
    let snapshot = workspace.snapshot(true)?;
    let Some(document) = snapshot.tasks.as_ref() else {
        return Ok(());
    };
    let apps: Vec<_> = snapshot.apps.values().cloned().collect();
    incremental.set_affected_graph_from_discovery(
        &apps,
        document,
        &snapshot.flake.nix_ref,
        &snapshot.nix.system,
    );
    Ok(())
}

fn seed_owned_outputs(caches: &mut WatchCaches) {
    let Some(task_plan) = caches.task_plan.as_ref() else {
        return;
    };
    let outputs: Vec<String> = task_plan
        .plan
        .serial_order
        .iter()
        .filter_map(|id| task_plan.document.tasks.get(id))
        .flat_map(|task| task.outputs.iter().map(|output| output.path.clone()))
        .collect();
    if let Some(incremental) = caches.incremental.as_mut() {
        incremental.set_owned_outputs(outputs);
    } else {
        caches.coalesce.set_owned_outputs(outputs);
    }
}

fn coalesce_pending_paths(
    watch_root: &camino::Utf8Path,
    caches: &mut WatchCaches,
    paths: Vec<Utf8PathBuf>,
) -> Vec<Utf8PathBuf> {
    if let Some(incremental) = caches.incremental.as_mut() {
        incremental.coalesce_pending_paths(paths)
    } else {
        caches.coalesce.coalesce_paths(watch_root, paths)
    }
}

fn apply_restart_classification(
    watch_root: &camino::Utf8Path,
    filters: &PathFilters,
    pending_changes: &[Utf8PathBuf],
    metadata_registry: &mut MetadataInputRegistry,
    workspace: &mut WorkspaceState<'_>,
    caches: &mut WatchCaches,
    runner: RunnerOutput,
) -> Result<bool, WatchCommandError> {
    let Some((merged, labeled)) =
        classify_pending_changes(watch_root, pending_changes, filters, metadata_registry)
    else {
        return Ok(false);
    };

    let invalidate_snapshot = merged == ChangeClass::Metadata;
    let snapshot_label = if invalidate_snapshot {
        "rebuilt"
    } else {
        "reused"
    };
    let plan_label = snapshot_label;

    let relative_paths: Vec<String> = labeled
        .iter()
        .map(|(path, _)| {
            path.strip_prefix(watch_root)
                .map_or_else(|_| path.as_str().to_owned(), |p| p.as_str().to_owned())
        })
        .collect();

    if merged == ChangeClass::Source
        && let Some(incremental) = caches.incremental.as_mut()
    {
        let plan_ids = caches
            .task_plan
            .as_ref()
            .map(|plan| plan.plan.serial_order.as_slice());
        let flake = workspace
            .snapshot(true)
            .map(|s| s.flake.nix_ref.clone())
            .unwrap_or_else(|_| watch_root.as_str().to_owned());
        let system = workspace
            .snapshot(false)
            .map(|s| s.nix.system.clone())
            .unwrap_or_default();
        let patch = incremental
            .apply_source_changes(&relative_paths, plan_ids, &flake, &system)
            .map_err(WatchCommandError::Io)?;
        if let Some(task_plan) = caches.task_plan.as_mut() {
            for id in &patch.affected_plan_nodes {
                task_plan.prepared_nodes.remove(id);
            }
        }
        runner
            .verbose(format!(
                "watch snapshot: patched {} path(s); dropped {} prepared node(s); action-digest entries removed: {}",
                patch.paths.len(),
                patch.affected_plan_nodes.len(),
                patch.action_digest_entries_removed
            ))
            .map_err(WatchCommandError::Io)?;
    }

    for (path, class) in &labeled {
        let relative = path
            .strip_prefix(watch_root)
            .map_or(path.as_str(), camino::Utf8Path::as_str);
        runner
            .verbose(format!(
                "change: {relative}\nclassification: {}\nsnapshot: {snapshot_label}\nplan: {plan_label}\ngeneration: restarted",
                change_class_label(*class)
            ))
            .map_err(WatchCommandError::Io)?;
    }

    // Wave 4a: notify optional nxrd so Merkle / warm state can drop ancestors.
    let _: Option<serde_json::Value> = nxr_core::try_once(
        "merkle.invalidate",
        Some(serde_json::json!({
            "root": watch_root.as_str(),
            "paths": relative_paths,
        })),
    );

    if invalidate_snapshot {
        workspace.invalidate_snapshots();
        caches.app_plan = None;
        caches.task_plan = None;
        caches.coalesce.clear_owned_outputs();
        if let Some(incremental) = caches.incremental.as_mut() {
            *incremental = WatchIncrementalSnapshot::new(watch_root.to_path_buf());
        }
    }

    Ok(invalidate_snapshot)
}

fn change_class_label(class: ChangeClass) -> &'static str {
    match class {
        ChangeClass::Metadata => "metadata",
        ChangeClass::Source => "source",
        ChangeClass::Ignored => "ignored",
    }
}

#[allow(clippy::too_many_arguments)]
fn run_generation(
    request: &WatchRequest<'_>,
    target: &WatchTarget,
    workspace: &mut WorkspaceState<'_>,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
    caches: &mut WatchCaches,
    invalidate_snapshot: bool,
    runner: RunnerOutput,
) -> Result<GenerationOutcome, WatchCommandError> {
    match target {
        WatchTarget::App { name } => {
            let app_request = AppRequest {
                flake_arg: request.flake_arg,
                nix_override: request.nix_override,
                app: name.as_str(),
                args: request.args,
                root: request.root,
                cwd: request.cwd,
                shell: request.shell,
                shell_mode: request.shell_mode,
                environment_policy: request.environment_policy.clone(),
                nix_flags: request.nix_flags,
                context: None,
            };
            let prepared = if let Some(cached) = caches.app_plan.as_ref() {
                if invalidate_snapshot {
                    let plan = prepare_app_plan_in_state(&app_request, workspace)?;
                    caches.app_plan = Some(plan.clone());
                    plan
                } else {
                    runner
                        .verbose("snapshot: reused\nplan: reused")
                        .map_err(WatchCommandError::Io)?;
                    cached.clone()
                }
            } else {
                let plan = prepare_app_plan_in_state(&app_request, workspace)?;
                caches.app_plan = Some(plan.clone());
                plan
            };
            let prewarm = caches
                .incremental
                .as_mut()
                .map(WatchIncrementalSnapshot::prewarm_mut);
            let supervisor = spawn_prepared(&prepared, prewarm)?;
            wait_supervisor(supervisor, session, interrupts)
        }
        WatchTarget::Task { name } => run_task_generation(
            request,
            name,
            workspace,
            session,
            interrupts,
            caches,
            invalidate_snapshot,
            runner,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_task_generation(
    request: &WatchRequest<'_>,
    resolved_name: &str,
    workspace: &mut WorkspaceState<'_>,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
    caches: &mut WatchCaches,
    invalidate_snapshot: bool,
    runner: RunnerOutput,
) -> Result<GenerationOutcome, WatchCommandError> {
    let single_root;
    let (tasks, jobs, keep_going, output_mode, events_format, reports, param_sets, log_dir) =
        if let Some(settings) = &request.task_settings {
            (
                settings.tasks.as_slice(),
                settings.jobs,
                settings.keep_going,
                settings.output_mode,
                settings.events_format,
                settings.reports.clone(),
                settings.param_sets.clone(),
                settings.log_dir.clone(),
            )
        } else {
            single_root = vec![resolved_name.to_owned()];
            (
                single_root.as_slice(),
                1,
                false,
                request.output_mode,
                request.events_format,
                ReportPaths::default(),
                std::collections::BTreeMap::new(),
                None,
            )
        };

    let task_request = TaskRequest {
        flake_arg: request.flake_arg,
        nix_override: request.nix_override,
        tasks: tasks.to_vec(),
        args: request.args,
        root: request.root,
        cwd: request.cwd,
        shell: request.shell,
        shell_mode: request.shell_mode,
        environment_policy: request.environment_policy.clone(),
        jobs,
        keep_going,
        output_mode,
        events_format,
        reports,
        nix_flags: request.nix_flags,
        context_override: None,
        refresh_discovery: false,
        param_sets,
        log_dir,
    };

    let reuse_from_cache = !invalidate_snapshot && caches.task_plan.is_some();
    if caches.task_plan.is_some() {
        seed_owned_outputs(caches);
    }
    if reuse_from_cache {
        reprepare_missing_task_nodes(request, &task_request, workspace, caches, runner)?;
    }
    let mut restart_requested = false;
    let prewarm = caches
        .incremental
        .as_mut()
        .map(WatchIncrementalSnapshot::prewarm_mut);
    let code = task::execute_with_control(
        &task_request,
        false,
        false,
        runner,
        &mut || {
            if interrupts.take_pending() {
                return Ok(task::RunControl::Stop);
            }
            session.drain_events();
            match session.poll_restart(Duration::ZERO) {
                Ok(WatchPoll::Restart) => {
                    restart_requested = true;
                    Ok(task::RunControl::Restart)
                }
                Ok(WatchPoll::Timeout) => Ok(task::RunControl::Continue),
                Err(error) => Err(io::Error::other(error)),
            }
        },
        if reuse_from_cache {
            None
        } else {
            Some(workspace)
        },
        reuse_from_cache,
        Some(&mut caches.task_plan),
        prewarm,
    )?;

    seed_owned_outputs(caches);
    if let (Some(incremental), Some(task_plan)) =
        (caches.incremental.as_mut(), caches.task_plan.as_ref())
    {
        sync_prewarm_nodes(incremental.prewarm_mut(), &task_plan.prepared_nodes);
    }

    if restart_requested {
        return Ok(GenerationOutcome::Restart);
    }
    if code == exit::INTERRUPTED {
        return Ok(GenerationOutcome::Stopped {
            code: exit::INTERRUPTED,
        });
    }
    Ok(GenerationOutcome::Idle)
}

fn reprepare_missing_task_nodes(
    _request: &WatchRequest<'_>,
    task_request: &TaskRequest<'_>,
    workspace: &mut WorkspaceState<'_>,
    caches: &mut WatchCaches,
    runner: RunnerOutput,
) -> Result<(), WatchCommandError> {
    let missing: Vec<String> = caches
        .task_plan
        .as_ref()
        .map(|task_cache| {
            task_cache
                .plan
                .serial_order
                .iter()
                .filter(|id| !task_cache.prepared_nodes.contains_key(*id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if missing.is_empty() {
        return Ok(());
    }

    let snapshot = workspace.snapshot(true)?;
    let digest_cache = caches
        .incremental
        .as_mut()
        .map(WatchIncrementalSnapshot::take_digest_cache)
        .unwrap_or_default();
    let context_hints = caches
        .incremental
        .as_ref()
        .map(|snapshot| {
            missing
                .iter()
                .filter_map(|id| {
                    snapshot
                        .prewarm()
                        .lookup_context(id)
                        .map(|ctx| (id.clone(), ctx.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let (prepared_nodes, digest_cache, hits, misses) = {
        let task_cache = caches
            .task_plan
            .as_mut()
            .expect("reprepare requires task plan cache");
        let existing = std::mem::take(&mut task_cache.prepared_nodes);
        let mut preparer = TaskNodePreparer::from_partial_prepared(
            existing,
            snapshot,
            &task_cache.document,
            &task_cache.canonical_roots,
            task_request.args,
            task_request.root,
            task_request.cwd,
            task_request.shell,
            task_request.shell_mode,
            &task_request.environment_policy,
            task_request.nix_flags,
            task_request.context_override.as_deref(),
            digest_cache,
            context_hints,
        )
        .map_err(WatchCommandError::Prepare)?;
        preparer
            .prepare_all(&missing)
            .map_err(WatchCommandError::Prepare)?;
        let (hits, misses) = preparer.take_prewarm_context_stats();
        let (prepared, digest_cache) = preparer.into_prepared_with_digest_cache();
        task_cache.prepared_nodes = prepared;
        (
            task_cache.prepared_nodes.clone(),
            digest_cache,
            hits,
            misses,
        )
    };

    if let Some(incremental) = caches.incremental.as_mut() {
        let prewarm = incremental.prewarm_mut();
        for _ in 0..hits {
            prewarm.record_context_hit();
        }
        for _ in 0..misses {
            prewarm.record_context_miss();
        }
        sync_prewarm_nodes(prewarm, &prepared_nodes);
        incremental.set_digest_cache(digest_cache);
    }
    runner
        .verbose(format!(
            "watch snapshot: reprepared {} affected node(s): {}",
            missing.len(),
            missing.join(", ")
        ))
        .map_err(WatchCommandError::Io)?;
    Ok(())
}

fn spawn_prepared(
    prepared: &PreparedPlan,
    prewarm: Option<&mut nxr_watch::WatchPrewarm>,
) -> Result<Supervisor, WatchCommandError> {
    let spawn = resolve_app_spawn_with_prewarm(
        &prepared.plan,
        &prepared.nix,
        prepared.local_root.as_deref(),
        &OptionalNixFlags::default(),
        "",
        Some(prepared.execution_directory.as_std_path()),
        prewarm,
    );
    let child = spawn_in(
        spawn.program.as_std_path(),
        &spawn.arguments,
        Some(prepared.execution_directory.as_std_path()),
        &prepared.plan.environment_policy,
    )
    .map_err(WatchCommandError::Supervision)?;
    let mut supervisor = Supervisor::new();
    supervisor.add("watch", child);
    Ok(supervisor)
}

fn wait_supervisor(
    mut supervisor: Supervisor,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
) -> Result<GenerationOutcome, WatchCommandError> {
    loop {
        if interrupts.take_pending() {
            let _ = supervisor
                .shutdown_all(SHUTDOWN_GRACE)
                .map_err(WatchCommandError::Supervision)?;
            return Ok(GenerationOutcome::Stopped {
                code: exit::INTERRUPTED,
            });
        }

        match session.poll_restart(Duration::from_millis(50))? {
            WatchPoll::Restart => {
                let _ = supervisor
                    .shutdown_all(SHUTDOWN_GRACE)
                    .map_err(WatchCommandError::Supervision)?;
                return Ok(GenerationOutcome::Restart);
            }
            WatchPoll::Timeout => {}
        }

        if let Some((_id, _code)) = supervisor
            .try_wait_any()
            .map_err(WatchCommandError::Supervision)?
        {
            return Ok(GenerationOutcome::Idle);
        }
    }
}

fn sync_prewarm_nodes(
    prewarm: &mut nxr_watch::WatchPrewarm,
    nodes: &std::collections::BTreeMap<String, crate::commands::common::PreparedTaskNode>,
) {
    for (id, node) in nodes {
        prewarm.store_context(
            id.clone(),
            PrewarmContext {
                context_name: node.context_name.clone(),
                confirm: node.confirm,
                environment_policy: node.environment.clone(),
                effective_shell: node.plan.shell.clone(),
                applied_context: None,
            },
        );
        if let Some(cache) = node.workspace_cache.as_ref() {
            prewarm.store_cas_handle(
                id.clone(),
                PrewarmCasHandle {
                    action_key_digest: cache.action_key.clone(),
                    workspace_cache: Some(cache.clone()),
                },
            );
        }
    }
}

fn clear_terminal() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\x1b[2J\x1b[H")?;
    stdout.flush()
}

/// Resolve name as task-first for unit tests of the preference rule.
#[must_use]
#[cfg(test)]
pub fn prefer_task_if_present(document: &nxr_task::TaskDocument, name: &str) -> bool {
    document.tasks.contains_key(name)
}

#[cfg(test)]
fn sample_task(app: &str) -> nxr_task::TaskDefinition {
    nxr_task::TaskDefinition::new(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn prefer_task_when_name_exists() {
        let mut tasks = BTreeMap::new();
        tasks.insert("ci".to_owned(), sample_task("ci"));
        let doc = nxr_task::TaskDocument::new(tasks);
        assert!(prefer_task_if_present(&doc, "ci"));
        assert!(!prefer_task_if_present(&doc, "hello"));
    }

    #[test]
    fn default_debounce_ms_is_300() {
        assert_eq!(DEFAULT_DEBOUNCE_MS, 300);
    }

    #[test]
    fn watch_options_default_matches_debounce_ms() {
        assert_eq!(
            WatchOptions::default().debounce,
            Duration::from_millis(DEFAULT_DEBOUNCE_MS)
        );
    }

    #[test]
    fn parse_watch_name_prefixes() {
        assert_eq!(
            parse_watch_name("app:hello"),
            WatchNameKind::App { name: "hello" }
        );
        assert_eq!(
            parse_watch_name("task:ci"),
            WatchNameKind::Task { name: "ci" }
        );
        assert_eq!(
            parse_watch_name("hello"),
            WatchNameKind::Unprefixed { name: "hello" }
        );
    }
}
