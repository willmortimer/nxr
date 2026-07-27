//! `nxr trust` project trust management.

use std::io::{self, Write};

use camino::Utf8Path;
use nxr_core::diagnostics::exit;
use nxr_core::{TrustDatabase, TrustError, enforce_project_trust, project_trust_key};
use serde::Serialize;

use crate::commands::common::current_invocation_directory;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while managing project trust.
#[derive(Debug, thiserror::Error)]
pub enum TrustCommandError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("failed to write trust output: {0}")]
    Io(#[source] io::Error),
}

impl TrustCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Trust(_) | Self::Json(_) => exit::EVALUATION,
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

#[derive(Serialize)]
struct TrustStatusJson {
    project_key: String,
    display: String,
    trusted: bool,
}

/// Show whether the selected project is trusted.
///
/// # Errors
///
/// Returns [`TrustCommandError`] when flake resolution or database access fails.
pub fn status(
    flake_arg: Option<&str>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), TrustCommandError> {
    let selection = resolve_trust_selection(flake_arg)?;
    let database = TrustDatabase::load()?;
    let trusted = database.is_trusted(&selection.project_key);
    if json {
        let payload = TrustStatusJson {
            project_key: selection.project_key,
            display: selection.display,
            trusted,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}").map_err(TrustCommandError::Io)?;
    } else if trusted {
        runner
            .info(format!("trusted: {}", selection.display))
            .map_err(TrustCommandError::Io)?;
    } else {
        runner
            .info(format!("not trusted: {}", selection.display))
            .map_err(TrustCommandError::Io)?;
    }
    Ok(())
}

/// Persist trust for the selected project.
///
/// # Errors
///
/// Returns [`TrustCommandError`] when flake resolution or database access fails.
pub fn add(flake_arg: Option<&str>, runner: RunnerOutput) -> Result<(), TrustCommandError> {
    let selection = resolve_trust_selection(flake_arg)?;
    let mut database = TrustDatabase::load()?;
    database.add_trust(&selection.project_key)?;
    runner
        .info(format!("trusted project: {}", selection.display))
        .map_err(TrustCommandError::Io)?;
    Ok(())
}

/// Remove persisted trust for the selected project.
///
/// # Errors
///
/// Returns [`TrustCommandError`] when flake resolution or database access fails.
pub fn revoke(flake_arg: Option<&str>, runner: RunnerOutput) -> Result<(), TrustCommandError> {
    let selection = resolve_trust_selection(flake_arg)?;
    let mut database = TrustDatabase::load()?;
    database.revoke_trust(&selection.project_key)?;
    runner
        .info(format!("revoked trust for project: {}", selection.display))
        .map_err(TrustCommandError::Io)?;
    Ok(())
}

/// Enforce project trust before executing secret-bearing or confirmation-gated tasks.
///
/// # Errors
///
/// Returns [`TrustCommandError`] when trust is required but missing.
pub fn enforce_for_execution(
    display: &str,
    local_root: Option<&Utf8Path>,
    nix_ref: &str,
) -> Result<(), TrustCommandError> {
    let project_key = project_trust_key(local_root.map(|path| path.as_std_path()), nix_ref);
    let database = TrustDatabase::load()?;
    enforce_project_trust(&project_key, display, &database)?;
    Ok(())
}

struct TrustSelection {
    project_key: String,
    display: String,
}

fn resolve_trust_selection(flake_arg: Option<&str>) -> Result<TrustSelection, TrustCommandError> {
    let invocation = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation)?;
    let project_key = project_trust_key(
        flake.local_root.as_deref().map(|path| path.as_std_path()),
        &flake.nix_ref,
    );
    Ok(TrustSelection {
        project_key,
        display: flake.display,
    })
}
