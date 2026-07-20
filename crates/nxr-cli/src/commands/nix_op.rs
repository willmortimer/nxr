//! Shared execution for `nxr build` / `nxr check` / `nxr shell`.

use std::io::{self, Write};

use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, FlakeOutput};
use nxr_nix::{
    NixAdapter, NixError, OptionalNixFlags, OutputNotFoundError, OutputTable, check_installable,
    package_installable, resolve_output_by_name,
};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while running a native flake-output command.
#[derive(Debug, thiserror::Error)]
pub enum NixOpError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    NotFound(#[from] OutputNotFoundError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl NixOpError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::NotFound(error) => error.exit_code(),
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NixOpKind {
    Build,
    Check,
    Shell,
}

impl NixOpKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Build => "package",
            Self::Check => "check",
            Self::Shell => "shell",
        }
    }
}

/// Shared inputs for build / check / shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NixOpRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub name: Option<&'a str>,
    pub dry_run: bool,
    pub json: bool,
    pub nix_flags: &'a OptionalNixFlags,
    pub environment: &'a EnvironmentPolicy,
}

#[derive(Serialize)]
struct DryRunEnvelope {
    schema_version: u32,
    kind: String,
    flake: String,
    system: String,
    target: Option<String>,
    attr_path: Option<String>,
    command: DryRunCommand,
}

#[derive(Serialize)]
struct DryRunCommand {
    program: String,
    arguments: Vec<String>,
}

fn discover_table(
    flake: &FlakeSelection,
    adapter: &NixAdapter,
    table: OutputTable,
    nix_flags: &OptionalNixFlags,
) -> Result<Vec<FlakeOutput>, NixOpError> {
    Ok(adapter.discover_outputs(&flake.nix_ref, table, nix_flags)?)
}

#[allow(clippy::too_many_arguments)]
fn write_dry_run(
    request: &NixOpRequest<'_>,
    kind: NixOpKind,
    flake: &FlakeSelection,
    system: &str,
    target: Option<&str>,
    attr_path: Option<&str>,
    nix: &str,
    arguments: &[String],
) -> Result<bool, NixOpError> {
    if !request.dry_run {
        return Ok(false);
    }

    let mut stdout = io::stdout().lock();
    if request.json {
        let envelope = DryRunEnvelope {
            schema_version: 1,
            kind: match kind {
                NixOpKind::Build => "build".to_owned(),
                NixOpKind::Check => "check".to_owned(),
                NixOpKind::Shell => "shell".to_owned(),
            },
            flake: flake.display.clone(),
            system: system.to_owned(),
            target: target.map(str::to_owned),
            attr_path: attr_path.map(str::to_owned),
            command: DryRunCommand {
                program: nix.to_owned(),
                arguments: arguments.to_vec(),
            },
        };
        writeln!(stdout, "{}", serde_json::to_string_pretty(&envelope)?)?;
    } else {
        write!(stdout, "{nix}")?;
        for arg in arguments {
            write!(stdout, " {arg}")?;
        }
        writeln!(stdout)?;
    }
    Ok(true)
}

fn run_nix_child(
    nix: &camino::Utf8Path,
    arguments: &[String],
    cwd: &camino::Utf8Path,
    environment: &EnvironmentPolicy,
) -> Result<i32, NixOpError> {
    nxr_process::run_in(
        nix.as_std_path(),
        arguments,
        Some(cwd.as_std_path()),
        environment,
    )
    .map_err(NixOpError::Io)
}

/// `nxr build [name]` → `nix build` for `packages.<system>.<name>` (or default package).
pub fn execute_build(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path, installable) = if let Some(name) = request.name {
        let outputs = discover_table(&flake, &adapter, OutputTable::Packages, request.nix_flags)?;
        let output = resolve_output_by_name(&outputs, name, NixOpKind::Build.label())?;
        (
            Some(output.name.clone()),
            Some(output.attr_path.clone()),
            package_installable(&flake.nix_ref, &adapter.system, name),
        )
    } else {
        (None, None, flake.nix_ref.clone())
    };

    let arguments = adapter.nix_build_argv(&installable, request.nix_flags)?;
    if write_dry_run(
        request,
        NixOpKind::Build,
        &flake,
        &adapter.system,
        target.as_deref(),
        attr_path.as_deref(),
        adapter.nix.as_str(),
        &arguments,
    )? {
        return Ok(exit::SUCCESS);
    }

    runner
        .verbose(format!("building {installable}"))
        .map_err(NixOpError::Io)?;
    run_nix_child(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )
}

/// `nxr check [name]` → named check via `nix build`, or `nix flake check` when omitted.
pub fn execute_check(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path, arguments) = if let Some(name) = request.name {
        let outputs = discover_table(&flake, &adapter, OutputTable::Checks, request.nix_flags)?;
        let output = resolve_output_by_name(&outputs, name, NixOpKind::Check.label())?;
        let installable = check_installable(&flake.nix_ref, &adapter.system, name);
        let arguments = adapter.nix_build_argv(&installable, request.nix_flags)?;
        (
            Some(output.name.clone()),
            Some(output.attr_path.clone()),
            arguments,
        )
    } else {
        let arguments = adapter.nix_flake_check_argv(&flake.nix_ref, request.nix_flags)?;
        (None, None, arguments)
    };

    if write_dry_run(
        request,
        NixOpKind::Check,
        &flake,
        &adapter.system,
        target.as_deref(),
        attr_path.as_deref(),
        adapter.nix.as_str(),
        &arguments,
    )? {
        return Ok(exit::SUCCESS);
    }

    let label = target
        .as_deref()
        .map_or_else(|| format!("flake check {}", flake.display), str::to_owned);
    runner
        .verbose(format!("checking {label}"))
        .map_err(NixOpError::Io)?;
    run_nix_child(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )
}

/// `nxr shell [name]` → interactive `nix develop` for a named (or default) shell.
pub fn execute_shell(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path) = if let Some(name) = request.name {
        let outputs = discover_table(&flake, &adapter, OutputTable::DevShells, request.nix_flags)?;
        let output = resolve_output_by_name(&outputs, name, NixOpKind::Shell.label())?;
        (Some(output.name.clone()), Some(output.attr_path.clone()))
    } else {
        (None, None)
    };

    let arguments = adapter.nix_develop_argv(&flake.nix_ref, request.name, request.nix_flags)?;
    if write_dry_run(
        request,
        NixOpKind::Shell,
        &flake,
        &adapter.system,
        target.as_deref(),
        attr_path.as_deref(),
        adapter.nix.as_str(),
        &arguments,
    )? {
        return Ok(exit::SUCCESS);
    }

    let label = request.name.unwrap_or("default");
    runner
        .verbose(format!("entering development shell {label}"))
        .map_err(NixOpError::Io)?;
    run_nix_child(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )
}
