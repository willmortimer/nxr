//! `nxr doctor builders` read-only remote builder diagnostics.

use std::io::{self, Write};

use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel, exit};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{
    OptionalNixFlags, config_string_list_setting, host_is_macos, probe_config_json, probe_nixd,
    redact_sensitive_text,
};
use serde::Serialize;

use crate::commands::common::WorkspaceState;
use crate::runner_output::RunnerOutput;

const SCHEMA_VERSION: u32 = 1;

/// Errors while running `nxr doctor builders`.
#[derive(Debug, thiserror::Error)]
pub enum DoctorBuildersError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DoctorBuildersError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned doctor builders report envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorBuildersReport {
    pub schema_version: u32,
    pub findings: Vec<Diagnostic>,
}

/// Inputs for `nxr doctor builders`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoctorBuildersRequest<'a> {
    pub nix_override: Option<&'a str>,
}

/// Run read-only builder diagnostics.
///
/// # Errors
///
/// Returns [`DoctorBuildersError`] when writing output fails.
pub fn run(
    request: DoctorBuildersRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DoctorBuildersError> {
    let mut findings = Vec::new();
    collect_findings(request, &mut findings);

    runner
        .info("running doctor builders diagnostics")
        .map_err(DoctorBuildersError::Io)?;

    let report = DoctorBuildersReport {
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

fn collect_findings(request: DoctorBuildersRequest<'_>, findings: &mut Vec<Diagnostic>) {
    let nix_flags = OptionalNixFlags::default();
    let mut workspace = WorkspaceState::new(None, request.nix_override, &nix_flags);

    let adapter = match workspace.adapter() {
        Ok(adapter) => adapter,
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "builders.nix.missing",
                error.user_message(),
            );
            return;
        }
    };

    push_finding(
        findings,
        DiagnosticLevel::Info,
        "builders.system.detected",
        format!("host system: {}", adapter.system),
    );

    let config_json = probe_config_json(&adapter.nix);
    if let Some(builders) = config_string_list_setting(config_json.as_deref(), "builders") {
        if builders.is_empty() {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "builders.configured.empty",
                "no remote builders configured".to_owned(),
            );
        } else {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "builders.configured",
                format!(
                    "remote builders configured ({}): {}",
                    builders.len(),
                    redact_sensitive_text(&builders.join("; "))
                ),
            );
        }
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "builders.configured.unavailable",
            "could not read builders from Nix configuration".to_owned(),
        );
    }

    if let Some(systems) = config_string_list_setting(config_json.as_deref(), "system") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "builders.systems.configured",
            format!("configured systems: {}", systems.join(", ")),
        );
    }

    collect_nixd_findings(findings);

    if host_is_macos(&adapter.system) {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "builders.host.macos",
            "macOS host: native Linux builders may be configured via Determinate Nixd".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "builders.host.linux",
            "Linux host: use remote builders or Determinate CI for cross-platform builds"
                .to_owned(),
        );
    }
}

fn collect_nixd_findings(findings: &mut Vec<Diagnostic>) {
    match probe_nixd() {
        Some(probe) => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "builders.nixd.found",
                format!("determinate-nixd found at {}", probe.executable),
            );
            if let Some(version) = probe.version {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "builders.nixd.version",
                    format!(
                        "determinate-nixd version: {}",
                        redact_sensitive_text(&version)
                    ),
                );
            }
            if let Some(status) = probe.status_summary {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "builders.nixd.status",
                    redact_sensitive_text(&status),
                );
            }
        }
        None => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "builders.nixd.missing",
                "determinate-nixd not found on PATH".to_owned(),
            );
        }
    }
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

fn write_human_report(writer: &mut impl Write, report: &DoctorBuildersReport) -> io::Result<()> {
    if report.findings.is_empty() {
        writeln!(writer, "doctor builders: no findings")?;
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

fn write_json_report(writer: &mut impl Write, report: &DoctorBuildersReport) -> io::Result<()> {
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(writer, "{rendered}")?;
    Ok(())
}
