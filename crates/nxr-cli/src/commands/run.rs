//! `nxr run` / bare-app execution.

use std::io;

use nxr_core::diagnostics::exit;

use crate::commands::common::{
    AppRequest, PrepareError, prepare_fast_app_plan, suggest_missing_app_after_run,
};
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::runner_output::RunnerOutput;

/// Errors while running an app.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Plan(#[from] PlanRenderError),
    #[error("failed to supervise child process: {0}")]
    Supervision(#[source] io::Error),
}

impl RunError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Plan(_) => exit::EVALUATION,
            Self::Supervision(_) => exit::PROCESS_SUPERVISION,
        }
    }
}

/// Resolve, optionally print a plan (`dry_run`), or execute the app in the foreground.
///
/// Uses the bare-app fast path: builds `nix run <flake>#<app>` without `flake show`.
/// On a nonzero Nix exit, optionally discovers apps to emit "did you mean?" when
/// the name is absent.
///
/// # Errors
///
/// Returns [`RunError`] when planning fails, plan rendering fails, or the child
/// cannot be supervised.
///
/// On success, returns the child exit code (or `0` for dry-run).
pub fn execute(
    request: &AppRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let prepared = prepare_fast_app_plan(request)?;

    if dry_run {
        let mut stdout = io::stdout().lock();
        write_plan(&mut stdout, &prepared.plan, json)?;
        return Ok(exit::SUCCESS);
    }

    runner
        .verbose(format!(
            "running app {} from {}",
            prepared.plan.target, prepared.plan.flake
        ))
        .map_err(RunError::Supervision)?;
    runner
        .verbose(format!(
            "execution directory: {}",
            prepared.execution_directory
        ))
        .map_err(RunError::Supervision)?;

    let code = nxr_process::run_in(
        prepared.nix.as_std_path(),
        &prepared.plan.command.arguments,
        Some(prepared.execution_directory.as_std_path()),
        &prepared.plan.environment_policy,
    )
    .map_err(RunError::Supervision)?;

    if code != exit::SUCCESS
        && let Ok(Some(not_found)) = suggest_missing_app_after_run(request)
    {
        return Err(RunError::Prepare(PrepareError::NotFound(not_found)));
    }

    Ok(code)
}
