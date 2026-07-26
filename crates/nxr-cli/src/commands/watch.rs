//! `nxr watch` — flake-root filesystem watch with kill+rerun generations.

use std::io::{self, Write};
use std::time::Duration;

use camino::Utf8PathBuf;
use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{AppNotFoundError, NixError, TaskDiscoveryError, resolve_app_by_name};
use nxr_process::{InterruptFlags, Supervisor, spawn_in};
use nxr_task::{PlanError, resolve_task_name};
use nxr_watch::{
    ChangeClass, Generation, MetadataInputRegistry, PathFilterError, PathFilters, WatchConfig,
    WatchError, WatchPoll, WatchSession, classify_pending_changes,
};

use crate::commands::common::{
    AppRequest, PrepareError, PreparedPlan, WorkspaceState, current_invocation_directory,
    prepare_app_plan_in_state,
};
use crate::commands::task::{self, PreparedTaskGeneration, TaskError, TaskRequest, plan_exit_code};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::output_task::{EventsFormat, TaskOutputMode};
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
    Task,
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
    let mut caches = WatchCaches::default();

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
            refresh_metadata_registry(&mut workspace, &mut metadata_registry)?;
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
        return Ok(WatchTarget::Task);
    }

    let snapshot = workspace.snapshot(true)?;
    let document = snapshot
        .tasks
        .as_ref()
        .expect("load_tasks=true always populates tasks");

    if resolve_task_name(document, request.name).is_ok() {
        Ok(WatchTarget::Task)
    } else {
        let apps: Vec<_> = snapshot.apps.values().cloned().collect();
        let app = resolve_app_by_name(&apps, request.name)?;
        Ok(WatchTarget::App {
            name: app.name.clone(),
        })
    }
}

fn refresh_metadata_registry(
    workspace: &mut WorkspaceState<'_>,
    registry: &mut MetadataInputRegistry,
) -> Result<(), WatchCommandError> {
    let snapshot = workspace.snapshot(true)?;
    if let Some(document) = snapshot.tasks.as_ref() {
        registry.set_discovery_inputs(document.discovery_inputs.clone());
    }
    Ok(())
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

    if invalidate_snapshot {
        workspace.invalidate_snapshots();
        caches.app_plan = None;
        caches.task_plan = None;
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
            let supervisor = spawn_prepared(&prepared)?;
            wait_supervisor(supervisor, session, interrupts)
        }
        WatchTarget::Task => run_task_generation(
            request,
            workspace,
            session,
            interrupts,
            caches,
            invalidate_snapshot,
            runner,
        ),
    }
}

fn run_task_generation(
    request: &WatchRequest<'_>,
    workspace: &mut WorkspaceState<'_>,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
    caches: &mut WatchCaches,
    invalidate_snapshot: bool,
    runner: RunnerOutput,
) -> Result<GenerationOutcome, WatchCommandError> {
    let single_root;
    let (tasks, jobs, keep_going, output_mode, events_format) =
        if let Some(settings) = &request.task_settings {
            (
                settings.tasks.as_slice(),
                settings.jobs,
                settings.keep_going,
                settings.output_mode,
                settings.events_format,
            )
        } else {
            single_root = vec![request.name.to_owned()];
            (
                single_root.as_slice(),
                1,
                false,
                request.output_mode,
                request.events_format,
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
        nix_flags: request.nix_flags,
    };

    let reuse_from_cache = !invalidate_snapshot && caches.task_plan.is_some();
    let mut restart_requested = false;
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
    )?;

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

fn spawn_prepared(prepared: &PreparedPlan) -> Result<Supervisor, WatchCommandError> {
    let child = spawn_in(
        prepared.nix.as_std_path(),
        &prepared.plan.command.arguments,
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
}
