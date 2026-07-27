//! `nxr fmt` — thin wrapper around `nix fmt` / the flake formatter.

use std::io::{self, Write};

use nxr_core::diagnostics::exit;
use nxr_nix::{NixError, OptionalNixFlags};
use serde::Serialize;

use crate::commands::common::{build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while running `nxr fmt`.
#[derive(Debug, thiserror::Error)]
pub enum FmtError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl FmtError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

/// Inputs for `nxr fmt`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FmtRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub paths: &'a [String],
    pub dry_run: bool,
    pub json: bool,
    pub nix_flags: &'a OptionalNixFlags,
}

#[derive(Serialize)]
struct FmtDryRunEnvelope {
    schema_version: u32,
    flake: String,
    paths: Vec<String>,
    command: FmtDryRunCommand,
}

#[derive(Serialize)]
struct FmtDryRunCommand {
    program: String,
    arguments: Vec<String>,
}

/// Run `nix fmt` for the selected flake and optional paths.
///
/// # Errors
///
/// Returns [`FmtError`] when flake resolution, Nix invocation, or output fails.
pub fn run(request: &FmtRequest<'_>, runner: RunnerOutput) -> Result<i32, FmtError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let mut paths: Vec<String> = request.paths.to_vec();
    if paths.is_empty() {
        paths.push(flake.nix_ref.clone());
    }

    let arguments = adapter.nix_fmt_argv(&paths, request.nix_flags)?;

    if request.dry_run {
        let mut stdout = io::stdout().lock();
        if request.json {
            let envelope = FmtDryRunEnvelope {
                schema_version: 1,
                flake: flake.display.clone(),
                paths,
                command: FmtDryRunCommand {
                    program: adapter.nix.as_str().to_owned(),
                    arguments: arguments.clone(),
                },
            };
            writeln!(stdout, "{}", serde_json::to_string_pretty(&envelope)?)?;
        } else {
            write!(stdout, "{}", adapter.nix)?;
            for arg in &arguments {
                write!(stdout, " {arg}")?;
            }
            writeln!(stdout)?;
        }
        return Ok(exit::SUCCESS);
    }

    runner
        .verbose(format!("formatting with flake {}", flake.display))
        .map_err(FmtError::Io)?;

    let code = nxr_process::run_in(
        adapter.nix.as_std_path(),
        &arguments,
        Some(invocation_cwd.as_std_path()),
        &nxr_core::EnvironmentPolicy::Inherit,
    )
    .map_err(FmtError::Io)?;

    Ok(code)
}
