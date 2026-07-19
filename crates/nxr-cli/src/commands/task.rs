//! `nxr task` serial execution.

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{NixError, TaskDiscoveryError};
use nxr_task::{PlanError, plan_serial};

use crate::commands::common::{
    AppRequest, PrepareError, build_adapter, current_invocation_directory,
};
use crate::commands::run::{self, RunError};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Inputs for serial task execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub task: &'a str,
    /// Forwarded only to the root task's app (MVP); dependency nodes get none.
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub environment_policy: EnvironmentPolicy,
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
    #[error("failed to write runner diagnostics: {0}")]
    Io(#[source] std::io::Error),
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
            Self::Io(_) => exit::PROCESS_SUPERVISION,
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

/// Discover tasks, plan a serial order, and run each node's app fail-fast.
///
/// Trailing `args` are forwarded only to the root task's app. Dependency nodes
/// receive an empty argument list (MVP; richer forwarding is deferred).
///
/// # Errors
///
/// Returns [`TaskError`] when flake resolution, discovery, planning, or app
/// preparation/supervision fails.
///
/// On success, returns the last child exit code (or the first nonzero on
/// fail-fast), or `0` when every node succeeds / dry-run completes.
pub fn execute(
    request: &TaskRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, TaskError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;
    let document = adapter.discover_tasks(&flake.nix_ref)?;

    let order = plan_serial(&document.tasks, request.task)?;

    runner
        .verbose(format!(
            "task plan for {}: {}",
            request.task,
            order.join(" -> ")
        ))
        .map_err(TaskError::Io)?;

    for task_id in &order {
        let definition = document
            .tasks
            .get(task_id)
            .expect("plan_serial only returns ids present in the document");
        let forwarded = if task_id.as_str() == request.task {
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

        runner
            .verbose(format!("running task {task_id} via app {}", definition.app))
            .map_err(TaskError::Io)?;

        let code = run::execute(&app_request, dry_run, json, runner)?;
        if code != exit::SUCCESS {
            return Ok(code);
        }
    }

    Ok(exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::plan_exit_code;
    use nxr_core::diagnostics::exit;
    use nxr_task::PlanError;

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
}
