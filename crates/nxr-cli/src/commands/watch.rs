//! `nxr watch` — flake-root filesystem watch with kill+rerun generations.

use std::io;
use std::time::Duration;

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{AppNotFoundError, NixError, TaskDiscoveryError, resolve_app_by_name};
use nxr_process::{ChildSession, InterruptFlags, spawn_in};
use nxr_task::{PlanError, TaskDocument, plan_serial, resolve_task_name};
use nxr_watch::{Generation, WatchConfig, WatchError, WatchPoll, WatchSession};

use crate::commands::common::{
    AppRequest, PrepareError, PreparedPlan, build_adapter, current_invocation_directory,
    prepare_app_plan,
};
use crate::commands::task::plan_exit_code;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Default debounce when the CLI omits `--debounce`.
pub const DEFAULT_DEBOUNCE_MS: u64 = 300;

/// Inputs for watch mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    /// App or task name (task wins when both exist).
    pub name: &'a str,
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub environment_policy: EnvironmentPolicy,
    pub debounce: Duration,
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
    NotFound(#[from] AppNotFoundError),
    #[error(transparent)]
    Watch(#[from] WatchError),
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
            Self::NotFound(error) => error.exit_code(),
            Self::Watch(_) | Self::RemoteFlake => exit::DISCOVERY,
            Self::Supervision(_) | Self::Io(_) => exit::PROCESS_SUPERVISION,
        }
    }
}

#[derive(Clone, Debug)]
enum WatchTarget {
    App {
        name: String,
    },
    Task {
        document: TaskDocument,
        root: String,
    },
}

enum GenerationOutcome {
    /// Target finished; wait for the next filesystem change.
    Idle,
    /// Filesystem change — start a new generation immediately.
    Restart,
    /// Ctrl-C / SIGTERM — stop watching.
    Stopped,
}

/// Resolve `name` as a task (preferred) or app, then watch the flake root.
///
/// # Errors
///
/// Returns [`WatchCommandError`] on resolution, watcher, or supervision failures.
///
/// On interrupt, returns success (`0`) after cleaning up the current generation.
pub fn run(request: &WatchRequest<'_>, runner: RunnerOutput) -> Result<i32, WatchCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let watch_root = flake
        .local_root
        .clone()
        .ok_or(WatchCommandError::RemoteFlake)?;

    let adapter = build_adapter(request.nix_override)?;
    let document = adapter.discover_tasks(&flake.nix_ref)?;

    let target = if let Ok(canonical) = resolve_task_name(&document, request.name) {
        let _order = plan_serial(&document.tasks, canonical)?;
        let root = canonical.to_owned();
        WatchTarget::Task { document, root }
    } else {
        let apps = adapter.discover_apps(&flake.nix_ref)?;
        let app = resolve_app_by_name(&apps, request.name)?;
        WatchTarget::App {
            name: app.name.clone(),
        }
    };

    let mut session = WatchSession::start(&WatchConfig {
        root: watch_root.clone(),
        debounce: request.debounce,
    })?;
    let interrupts = InterruptFlags::install().map_err(WatchCommandError::Supervision)?;
    let mut generation = Generation::new();

    runner
        .info(format!(
            "watching {} for changes (debounce {}ms); Ctrl-C to stop",
            watch_root,
            request.debounce.as_millis()
        ))
        .map_err(WatchCommandError::Io)?;

    loop {
        let generation_id = generation.bump();
        runner
            .verbose(format!("watch generation {generation_id}"))
            .map_err(WatchCommandError::Io)?;

        session.drain_events();

        match run_generation(request, &target, &mut session, &interrupts, runner)? {
            GenerationOutcome::Idle => loop {
                if interrupts.take_pending() {
                    return Ok(exit::SUCCESS);
                }
                match session.poll_restart(Duration::from_millis(100))? {
                    WatchPoll::Restart => break,
                    WatchPoll::Timeout => {}
                }
            },
            GenerationOutcome::Restart => {}
            GenerationOutcome::Stopped => return Ok(exit::SUCCESS),
        }
    }
}

fn run_generation(
    request: &WatchRequest<'_>,
    target: &WatchTarget,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
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
                environment_policy: request.environment_policy.clone(),
            };
            let prepared = prepare_app_plan(&app_request)?;
            let child = spawn_prepared(&prepared)?;
            wait_session(child, session, interrupts)
        }
        WatchTarget::Task { document, root } => {
            let order = plan_serial(&document.tasks, root)?;
            for task_id in &order {
                if interrupts.take_pending() {
                    return Ok(GenerationOutcome::Stopped);
                }
                session.drain_events();
                if matches!(session.poll_restart(Duration::ZERO)?, WatchPoll::Restart) {
                    return Ok(GenerationOutcome::Restart);
                }

                let definition = document
                    .tasks
                    .get(task_id)
                    .expect("plan_serial only returns ids present in the document");
                let forwarded = if task_id.as_str() == root.as_str() {
                    request.args
                } else {
                    &[]
                };
                runner
                    .verbose(format!("watch task {task_id} via app {}", definition.app))
                    .map_err(WatchCommandError::Io)?;

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
                let prepared = prepare_app_plan(&app_request)?;
                let child = spawn_prepared(&prepared)?;
                match wait_session(child, session, interrupts)? {
                    GenerationOutcome::Idle => {}
                    other => return Ok(other),
                }
            }
            Ok(GenerationOutcome::Idle)
        }
    }
}

fn spawn_prepared(prepared: &PreparedPlan) -> Result<ChildSession, WatchCommandError> {
    spawn_in(
        prepared.nix.as_std_path(),
        &prepared.plan.command.arguments,
        Some(prepared.execution_directory.as_std_path()),
        &prepared.plan.environment_policy,
    )
    .map_err(WatchCommandError::Supervision)
}

fn wait_session(
    mut child: ChildSession,
    session: &mut WatchSession,
    interrupts: &InterruptFlags,
) -> Result<GenerationOutcome, WatchCommandError> {
    loop {
        if interrupts.take_pending() {
            let _ = child.terminate().map_err(WatchCommandError::Supervision)?;
            return Ok(GenerationOutcome::Stopped);
        }

        match session.poll_restart(Duration::from_millis(50))? {
            WatchPoll::Restart => {
                let _ = child.terminate().map_err(WatchCommandError::Supervision)?;
                return Ok(GenerationOutcome::Restart);
            }
            WatchPoll::Timeout => {}
        }

        if let Some(_code) = child.try_wait().map_err(WatchCommandError::Supervision)? {
            return Ok(GenerationOutcome::Idle);
        }
    }
}

/// Resolve name as task-first for unit tests of the preference rule.
#[must_use]
#[cfg(test)]
pub fn prefer_task_if_present(document: &TaskDocument, name: &str) -> bool {
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
        let doc = TaskDocument::new(tasks);
        assert!(prefer_task_if_present(&doc, "ci"));
        assert!(!prefer_task_if_present(&doc, "hello"));
    }

    #[test]
    fn default_debounce_ms_is_300() {
        assert_eq!(DEFAULT_DEBOUNCE_MS, 300);
    }
}
