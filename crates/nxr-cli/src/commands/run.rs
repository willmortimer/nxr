//! `nxr run` / bare-app execution.

use std::io;

use nxr_core::diagnostics::exit;

use crate::commands::common::{AppRequest, PrepareError, prepare_app_plan};
use crate::commands::plan::{PlanRenderError, write_plan};

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
/// # Errors
///
/// Returns [`RunError`] when planning fails, plan rendering fails, or the child
/// cannot be supervised.
///
/// On success, returns the child exit code (or `0` for dry-run).
pub fn execute(request: AppRequest<'_>, dry_run: bool, json: bool) -> Result<i32, RunError> {
    let prepared = prepare_app_plan(request)?;

    if dry_run {
        let mut stdout = io::stdout().lock();
        write_plan(&mut stdout, &prepared.plan, json)?;
        return Ok(exit::SUCCESS);
    }

    let code = nxr_process::run_in(
        prepared.nix.as_std_path(),
        &prepared.plan.command.arguments,
        Some(prepared.execution_directory.as_std_path()),
    )
    .map_err(RunError::Supervision)?;

    Ok(code)
}
