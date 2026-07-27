//! `nxr doctor env` read-only direnv and shell-integration diagnostics.

use std::io::{self, Write};

use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel, exit};
use nxr_core::sanitize::sanitize_terminal_text;
use serde::Serialize;

use crate::commands::common::current_invocation_directory;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;
use crate::shell_mode::{NXR_DEV_SHELL_ENV, active_dev_shell};

const SCHEMA_VERSION: u32 = 1;

/// Errors while running `nxr doctor env`.
#[derive(Debug, thiserror::Error)]
pub enum DoctorEnvError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DoctorEnvError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned doctor env report envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorEnvReport {
    pub schema_version: u32,
    pub findings: Vec<Diagnostic>,
}

/// Inputs for `nxr doctor env`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoctorEnvRequest<'a> {
    pub flake_arg: Option<&'a str>,
}

/// Run read-only direnv and shell-integration diagnostics.
///
/// # Errors
///
/// Returns [`DoctorEnvError`] when writing output fails.
pub fn run(
    request: DoctorEnvRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DoctorEnvError> {
    let mut findings = Vec::new();
    collect_findings(request, &mut findings)?;

    runner
        .info("running doctor env diagnostics")
        .map_err(DoctorEnvError::Io)?;

    let report = DoctorEnvReport {
        schema_version: SCHEMA_VERSION,
        findings,
    };
    let exit_code = exit_code_for_findings(&report.findings);

    let mut stdout = io::stdout().lock();
    if json {
        write_json_report(&mut stdout, &report)?;
    } else {
        write_human_report(&mut stdout, &report)?;
    }

    Ok(exit_code)
}

#[allow(clippy::too_many_lines)]
fn collect_findings(
    request: DoctorEnvRequest<'_>,
    findings: &mut Vec<Diagnostic>,
) -> Result<(), DoctorEnvError> {
    if std::process::Command::new("direnv")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "env.direnv.found",
            "direnv found on PATH".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "env.direnv.missing",
            "direnv not found on PATH".to_owned(),
        );
    }

    if std::env::var("DIRENV_DIR").is_ok() || std::env::var("DIRENV_WATCHES").is_ok() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "env.direnv.loaded",
            "direnv appears active in this shell".to_owned(),
        );
    }

    for key in [
        "NIX_DIRENV_DID_FALLBACK",
        "NIX_DIRENV_DID_LOAD",
        "NIX_DIRENV_DID_RELOAD",
    ] {
        if let Ok(value) = std::env::var(key) {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "env.nix_direnv.var",
                format!("{key}={value}"),
            );
        }
    }

    if let Ok(value) = std::env::var("NIX_DIRENV_DID_FALLBACK")
        && (value == "1" || value.eq_ignore_ascii_case("true"))
    {
        push_finding(
            findings,
            DiagnosticLevel::Warning,
            "env.nix_direnv.fallback",
            "nix-direnv loaded its previous working environment because the current \
             devShell failed evaluation"
                .to_owned(),
        );
    }

    if let Some(active) = active_dev_shell() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "env.nxr_dev_shell.active",
            format!("{NXR_DEV_SHELL_ENV}={active}"),
        );
    }

    if std::env::var("NXR_SHELL_INTEGRATION").is_ok() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "env.nxr_shell_integration.loaded",
            "nxr shell integration appears loaded".to_owned(),
        );
    }

    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;

    if let Some(root) = flake.local_root.as_ref() {
        let envrc = root.join(".envrc");
        if envrc.is_file() {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "env.envrc.present",
                format!(".envrc present at {envrc}"),
            );
        } else {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "env.envrc.missing",
                format!(".envrc not found at {envrc}"),
            );
        }

        for tracked in ["flake.nix", "flake.lock"] {
            let path = root.join(tracked);
            if path.is_file() {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "env.flake.tracked",
                    format!("{tracked} present at flake root"),
                );
            }
        }

        if root.join("devenv.yaml").is_file() || root.join("devenv.nix").is_file() {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "env.devenv.detected",
                "devenv metadata detected (read-only; nxr does not activate devenv)".to_owned(),
            );
        }
    }

    Ok(())
}

fn push_finding(
    findings: &mut Vec<Diagnostic>,
    level: DiagnosticLevel,
    code: &str,
    message: String,
) {
    findings.push(Diagnostic {
        level,
        code: code.to_owned(),
        message,
    });
}

fn exit_code_for_findings(findings: &[Diagnostic]) -> i32 {
    if findings
        .iter()
        .any(|finding| finding.level == DiagnosticLevel::Error)
    {
        exit::CHILD_FAILED
    } else {
        exit::SUCCESS
    }
}

fn write_human_report(writer: &mut impl Write, report: &DoctorEnvReport) -> io::Result<()> {
    if report.findings.is_empty() {
        writeln!(writer, "doctor env: no findings")?;
        return Ok(());
    }

    for finding in &report.findings {
        let level = match finding.level {
            DiagnosticLevel::Info => "info",
            DiagnosticLevel::Warning => "warning",
            DiagnosticLevel::Error => "error",
        };
        writeln!(
            writer,
            "{level}: {}: {}",
            finding.code,
            sanitize_terminal_text(&finding.message)
        )?;
    }

    Ok(())
}

fn write_json_report(writer: &mut impl Write, report: &DoctorEnvReport) -> io::Result<()> {
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(writer, "{rendered}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DoctorEnvReport, write_json_report};
    use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel};

    #[test]
    fn json_report_includes_stable_codes() {
        let report = DoctorEnvReport {
            schema_version: 1,
            findings: vec![Diagnostic {
                level: DiagnosticLevel::Info,
                code: "env.direnv.missing".to_owned(),
                message: "direnv not found on PATH".to_owned(),
            }],
        };
        let mut output = Vec::new();
        write_json_report(&mut output, &report).expect("write json");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("env.direnv.missing"));
    }
}
