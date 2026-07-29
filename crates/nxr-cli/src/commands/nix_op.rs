//! Shared execution for `nxr build` / `nxr check` / `nxr shell`.

use std::io::{self, Write};

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{
    NixError, NixProgressFormatter, NixProgressMode, OptionalNixFlags, OutputNotFoundError,
    OutputTable, attr_installable, check_installable, ensure_internal_json_log_format, locate_nom,
    package_installable, resolve_output_by_name, token_is_explicit_installable,
    write_progress_line,
};
use serde::Serialize;

use crate::commands::common::{
    PrepareError, build_adapter, current_invocation_directory, stderr_indicates_missing_installable,
};
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
    #[error(transparent)]
    Configuration(#[from] super::configurations::ConfigurationError),
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
            Self::Configuration(error) => error.exit_code(),
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
    #[allow(dead_code)]
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
    pub attr: Option<&'a str>,
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

fn run_nix_child_with_stderr(
    nix: &camino::Utf8Path,
    arguments: &[String],
    cwd: &camino::Utf8Path,
    environment: &EnvironmentPolicy,
) -> Result<(i32, String), NixOpError> {
    match NixProgressMode::from_env() {
        NixProgressMode::Off => nxr_process::run_in_with_stderr(
            nix.as_std_path(),
            arguments,
            Some(cwd.as_std_path()),
            environment,
            None,
        )
        .map_err(NixOpError::Io),
        NixProgressMode::Nom => {
            if let Some(nom) = locate_nom() {
                // `nom build …` mirrors `nix build …` argv; nom formats progress itself.
                nxr_process::run_in_with_stderr(
                    &nom,
                    arguments,
                    Some(cwd.as_std_path()),
                    environment,
                    None,
                )
                .map_err(NixOpError::Io)
            } else {
                run_nix_child_with_builtin_progress(nix, arguments, cwd, environment)
            }
        }
        NixProgressMode::Builtin => {
            run_nix_child_with_builtin_progress(nix, arguments, cwd, environment)
        }
    }
}

fn run_nix_child_with_builtin_progress(
    nix: &camino::Utf8Path,
    arguments: &[String],
    cwd: &camino::Utf8Path,
    environment: &EnvironmentPolicy,
) -> Result<(i32, String), NixOpError> {
    use std::io::{self, IsTerminal, Write};

    let mut args = arguments.to_vec();
    ensure_internal_json_log_format(&mut args);
    let is_tty = io::stderr().is_terminal();
    let mut formatter = NixProgressFormatter::new();

    nxr_process::run_in_with_stderr_lines(
        nix.as_std_path(),
        &args,
        Some(cwd.as_std_path()),
        environment,
        None,
        move |line| {
            if let Some(rendered) = formatter.feed_line(line) {
                let mut stderr = io::stderr().lock();
                let _ = write_progress_line(&mut stderr, &rendered, is_tty);
            }
        },
    )
    .map_err(NixOpError::Io)
    .map(|(code, stderr)| {
        if is_tty {
            let mut out = io::stderr().lock();
            let _ = write!(out, "\r\x1b[2K");
            let _ = out.flush();
        }
        (code, stderr)
    })
}

/// After a failed direct installable, discover outputs and map missing names to suggestions.
fn suggest_missing_output(
    adapter: &nxr_nix::NixAdapter,
    flake_ref: &str,
    name: &str,
    table: OutputTable,
    kind: &str,
    nix_flags: &OptionalNixFlags,
) -> Result<Option<OutputNotFoundError>, NixOpError> {
    let outputs = adapter.discover_outputs(flake_ref, table, nix_flags)?;
    match resolve_output_by_name(&outputs, name, kind) {
        Ok(_) => Ok(None),
        Err(error) => Ok(Some(error)),
    }
}

/// `nxr build [installable]` → `nix build` for packages or explicit installables.
///
/// Named builds use a direct installable (no whole-output discovery up front).
/// Suggestion discovery runs only when stderr indicates a missing attribute.
pub fn execute_build(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path, installable) = resolve_build_target(request, &flake, &adapter);

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
    let (code, stderr) = run_nix_child_with_stderr(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )?;

    if code != exit::SUCCESS
        && let Some(name) = request.name
        && !token_is_explicit_installable(name)
        && request.attr.is_none()
        && stderr_indicates_missing_installable(&stderr)
        && let Ok(Some(not_found)) = suggest_missing_output(
            &adapter,
            &flake.nix_ref,
            name,
            OutputTable::Packages,
            "package",
            request.nix_flags,
        )
    {
        return Err(NixOpError::NotFound(not_found));
    }

    Ok(code)
}

/// `nxr build configuration <name>` → read-only configuration build installable.
pub fn execute_build_configuration(
    request: &NixOpRequest<'_>,
    configuration_name: &str,
    runner: RunnerOutput,
) -> Result<i32, NixOpError> {
    let (flake, _entry, installable) = super::configurations::resolve_for_build(
        request.flake_arg,
        request.nix_override,
        configuration_name,
        request.nix_flags,
    )?;
    let adapter = build_adapter(request.nix_override)?;
    let attr_path = installable.split_once('#').map(|(_, attr)| attr.to_owned());

    let arguments = adapter.nix_build_argv(&installable, request.nix_flags)?;
    if write_dry_run(
        request,
        NixOpKind::Build,
        &flake,
        &adapter.system,
        Some(configuration_name),
        attr_path.as_deref(),
        adapter.nix.as_str(),
        &arguments,
    )? {
        return Ok(exit::SUCCESS);
    }

    runner
        .verbose(format!("building configuration {configuration_name}"))
        .map_err(NixOpError::Io)?;
    let invocation_cwd = current_invocation_directory()?;
    let (code, _) = run_nix_child_with_stderr(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )?;
    Ok(code)
}

fn resolve_build_target(
    request: &NixOpRequest<'_>,
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
) -> (Option<String>, Option<String>, String) {
    if let Some(attr) = request.attr {
        let attr_path = attr.to_owned();
        let installable = attr_installable(&flake.nix_ref, attr);
        return (None, Some(attr_path), installable);
    }

    if let Some(name) = request.name {
        if token_is_explicit_installable(name) {
            let attr_path = name
                .split_once('#')
                .map(|(_, attr)| attr.to_owned())
                .or_else(|| Some(name.to_owned()));
            return (None, attr_path, name.to_owned());
        }

        let attr_path = format!("packages.{}.{name}", adapter.system);
        let installable = package_installable(&flake.nix_ref, &adapter.system, name);
        return (Some(name.to_owned()), Some(attr_path), installable);
    }

    (None, None, flake.nix_ref.clone())
}

/// `nxr check [name]` → named check via `nix build`, or `nix flake check` when omitted.
pub fn execute_check(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path, arguments) = if let Some(name) = request.name {
        let installable = check_installable(&flake.nix_ref, &adapter.system, name);
        let arguments = adapter.nix_build_argv(&installable, request.nix_flags)?;
        (
            Some(name.to_owned()),
            Some(format!("checks.{}.{name}", adapter.system)),
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
    let (code, stderr) = run_nix_child_with_stderr(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )?;

    if code != exit::SUCCESS
        && let Some(name) = request.name
        && stderr_indicates_missing_installable(&stderr)
        && let Ok(Some(not_found)) = suggest_missing_output(
            &adapter,
            &flake.nix_ref,
            name,
            OutputTable::Checks,
            "check",
            request.nix_flags,
        )
    {
        return Err(NixOpError::NotFound(not_found));
    }

    Ok(code)
}

/// `nxr shell [name]` → interactive `nix develop` for a named (or default) shell.
pub fn execute_shell(request: &NixOpRequest<'_>, runner: RunnerOutput) -> Result<i32, NixOpError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;

    let (target, attr_path) = if let Some(name) = request.name {
        (
            Some(name.to_owned()),
            Some(format!("devShells.{}.{name}", adapter.system)),
        )
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
    let (code, stderr) = run_nix_child_with_stderr(
        &adapter.nix,
        &arguments,
        &invocation_cwd,
        request.environment,
    )?;

    if code != exit::SUCCESS
        && let Some(name) = request.name
        && stderr_indicates_missing_installable(&stderr)
        && let Ok(Some(not_found)) = suggest_missing_output(
            &adapter,
            &flake.nix_ref,
            name,
            OutputTable::DevShells,
            "shell",
            request.nix_flags,
        )
    {
        return Err(NixOpError::NotFound(not_found));
    }

    Ok(code)
}
