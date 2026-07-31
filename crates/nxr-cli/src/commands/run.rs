//! `nxr run` / bare-app execution.

use std::io;

use nxr_core::diagnostics::exit;

use crate::commands::common::{
    AppRequest, PrepareError, prepare_fast_app_plan, stderr_indicates_missing_installable,
    suggest_missing_app_after_run,
};
use crate::commands::history;
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::script::{self, LiveFastPathOutcome, resolve_live_file_backed_app};
use crate::commands::store_exe::resolve_app_spawn;
use crate::osc52::maybe_emit_app_failure_clipboard;
use crate::runner_output::RunnerOutput;

/// Errors while running an app.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Plan(#[from] PlanRenderError),
    #[error(transparent)]
    Script(#[from] script::ScriptError),
    #[error("failed to supervise child process: {0}")]
    Supervision(#[source] io::Error),
}

impl RunError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Plan(_) => exit::EVALUATION,
            Self::Script(error) => error.exit_code(),
            Self::Supervision(_) => exit::PROCESS_SUPERVISION,
        }
    }
}

/// Resolve, optionally print a plan (`dry_run`), or execute the app in the foreground.
///
/// Uses the bare-app fast path: builds `nix run <flake>#<app>` without probes or
/// `flake show`. Suggestion discovery runs only when stderr indicates a missing
/// installable — not after ordinary app nonzero exits.
///
/// When a local file-backed app opts into `fastPath.enable` (ADR-0170) and warm
/// discovery metadata is available (or one cold listing eval succeeds), may spawn
/// the live workspace script instead. Remote flakes never take that path.
///
/// When the store-exe cache hits (ADR-0153), spawns the realised store program
/// directly; dry-run still renders the `nix run` plan (escape hatch unchanged)
/// unless the live fast path was selected.
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
    let started = std::time::Instant::now();

    if let LiveFastPathOutcome::Hit(live) =
        resolve_live_file_backed_app(request, true).map_err(RunError::Script)?
    {
        if dry_run {
            let mut stdout = io::stdout().lock();
            write_plan(&mut stdout, &live.plan, json)?;
            return Ok(exit::SUCCESS);
        }

        let code = script::execute_prepared_script(&live, request.app, runner)
            .map_err(RunError::Script)?;

        history::record_completed_run(
            started,
            nxr_core::RunTargetKind::WorkspaceScript,
            request.app.to_owned(),
            Some(live.plan.flake.clone()),
            code,
            None,
            false,
        );
        maybe_emit_app_failure_clipboard(request.app, code);
        return Ok(code);
    }

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

    let spawn = resolve_app_spawn(
        &prepared.plan,
        &prepared.nix,
        prepared.local_root.as_deref(),
        request.nix_flags,
        "",
        Some(prepared.execution_directory.as_std_path()),
    );
    if spawn.used_store_exe {
        runner
            .verbose(format!("store-exe: {}", spawn.program))
            .map_err(RunError::Supervision)?;
    }

    let (code, stderr) = nxr_process::run_in_with_stderr(
        spawn.program.as_std_path(),
        &spawn.arguments,
        Some(prepared.execution_directory.as_std_path()),
        &prepared.plan.environment_policy,
        None,
    )
    .map_err(RunError::Supervision)?;

    if code != exit::SUCCESS
        && !spawn.used_store_exe
        && stderr_indicates_missing_installable(&stderr)
        && let Ok(Some(not_found)) = suggest_missing_app_after_run(request)
    {
        return Err(RunError::Prepare(PrepareError::NotFound(not_found)));
    }

    history::record_completed_run(
        started,
        nxr_core::RunTargetKind::App,
        request.app.to_owned(),
        Some(prepared.plan.flake.clone()),
        code,
        None,
        false,
    );

    maybe_emit_app_failure_clipboard(request.app, code);

    Ok(code)
}
