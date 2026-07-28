//! `nxr run` / bare-app execution.

use std::io;

use nxr_completion::cached_workspace_best_effort;
use nxr_core::diagnostics::exit;

use crate::commands::common::{
    AppRequest, PrepareError, current_invocation_directory, prepare_fast_app_plan,
    resolve_execution_directory, stderr_indicates_missing_installable, strip_one_separator,
    suggest_missing_app_after_run,
};
use crate::commands::history;
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::script::{self, prepare_live_file_app};
use crate::commands::store_exe::resolve_app_spawn;
use crate::flake::resolve_flake;
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
/// discovery metadata is available, may spawn the live workspace script instead.
/// Remote flakes never take that path. Cache misses fall through to the store app
/// (warm with `nxr list` / task discovery).
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

    if let Some(live) = try_live_file_backed_fast_path(request)? {
        if dry_run {
            let mut stdout = io::stdout().lock();
            write_plan(&mut stdout, &live.plan, json)?;
            return Ok(exit::SUCCESS);
        }

        runner
            .verbose(format!(
                "running live workspace script {} (fallback app {})",
                live.script_path, request.app
            ))
            .map_err(RunError::Supervision)?;

        let (code, _stderr) = nxr_process::run_in_with_stderr(
            live.program.as_std_path(),
            &live.arguments,
            Some(live.execution_directory.as_std_path()),
            &live.plan.environment_policy,
        )
        .map_err(RunError::Supervision)?;

        history::record_completed_run(
            started,
            nxr_core::RunTargetKind::WorkspaceScript,
            request.app.to_owned(),
            Some(live.plan.flake.clone()),
            code,
            None,
            false,
        );
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

    Ok(code)
}

/// Peek warm discovery metadata only — never cold-eval Nix for this decision.
fn try_live_file_backed_fast_path(
    request: &AppRequest<'_>,
) -> Result<Option<script::PreparedScript>, RunError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd).map_err(PrepareError::from)?;
    let Some(local_root) = flake.local_root.as_ref() else {
        return Ok(None);
    };

    let Some(cached) = cached_workspace_best_effort(local_root) else {
        return Ok(None);
    };
    let Some(document) = cached.tasks.as_ref() else {
        return Ok(None);
    };
    let Some(listing) = document.apps.get(request.app) else {
        return Ok(None);
    };
    let Some(fast_path) = listing.fast_path.as_ref() else {
        return Ok(None);
    };
    if !fast_path.enable {
        return Ok(None);
    }
    let Some(workspace_path) = listing.workspace_path.as_deref() else {
        return Ok(None);
    };

    let script_path = local_root.join(workspace_path);
    if !script_path.is_file() {
        return Ok(None);
    }

    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let forwarded = strip_one_separator(request.args);
    let prepared = prepare_live_file_app(
        request.shell,
        request.shell_mode,
        request.environment_policy.clone(),
        request.nix_override,
        request.nix_flags,
        &flake,
        local_root,
        &invocation_cwd,
        &execution_directory,
        request.app,
        workspace_path,
        listing.interpreter.as_deref(),
        fast_path.shell.as_deref(),
        &forwarded,
    )?;
    Ok(Some(prepared))
}
