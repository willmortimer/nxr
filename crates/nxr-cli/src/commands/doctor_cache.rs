//! `nxr doctor cache` read-only Nix cache and nxr discovery diagnostics.

use std::io::{self, Write};

use nxr_completion::cache::{DiscoveryContext, discovery_cache_entry, discovery_cache_status};
use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel, exit};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{
    NixAdapter, OptionalNixFlags, capability_cache_status, config_string_list_setting,
    probe_config_json, redact_sensitive_text,
};
use serde::Serialize;

use crate::commands::common::{WorkspaceState, current_invocation_directory};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

const SCHEMA_VERSION: u32 = 1;

/// Errors while running `nxr doctor cache`.
#[derive(Debug, thiserror::Error)]
pub enum DoctorCacheError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DoctorCacheError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned doctor cache report envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCacheReport {
    pub schema_version: u32,
    pub findings: Vec<Diagnostic>,
}

/// Inputs for `nxr doctor cache`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoctorCacheRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
}

/// Run read-only cache and substituter diagnostics.
///
/// # Errors
///
/// Returns [`DoctorCacheError`] when writing output fails.
pub fn run(
    request: DoctorCacheRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DoctorCacheError> {
    let mut findings = Vec::new();
    collect_findings(request, &mut findings)?;

    runner
        .info("running doctor cache diagnostics")
        .map_err(DoctorCacheError::Io)?;

    let report = DoctorCacheReport {
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

fn collect_findings(
    request: DoctorCacheRequest<'_>,
    findings: &mut Vec<Diagnostic>,
) -> Result<(), DoctorCacheError> {
    let nix_flags = OptionalNixFlags::default();
    let mut workspace = WorkspaceState::new(request.flake_arg, request.nix_override, &nix_flags);

    let adapter = match workspace.adapter() {
        Ok(adapter) => adapter,
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "cache.nix.missing",
                error.user_message(),
            );
            return Ok(());
        }
    };

    let config_json = probe_config_json(&adapter.nix);
    collect_nix_cache_config_findings(config_json.as_deref(), findings);
    collect_nxr_cache_findings(request, adapter, findings)?;
    collect_capability_cache_findings(findings);

    Ok(())
}

fn collect_nix_cache_config_findings(config_json: Option<&str>, findings: &mut Vec<Diagnostic>) {
    if let Some(substituters) = config_string_list_setting(config_json, "substituters") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.substituters.configured",
            format!(
                "Nix substituters ({}): {}",
                substituters.len(),
                redact_sensitive_text(&substituters.join(" "))
            ),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.substituters.unavailable",
            "could not read substituters from Nix configuration".to_owned(),
        );
    }

    if let Some(keys) = config_string_list_setting(config_json, "trusted-public-keys") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.trusted_keys.configured",
            format!("trusted public keys: {}", keys.len()),
        );
    }

    if let Some(keys) = config_string_list_setting(config_json, "extra-substituters") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.extra_substituters.configured",
            format!("extra substituters: {}", keys.len()),
        );
    }
}

fn collect_nxr_cache_findings(
    request: DoctorCacheRequest<'_>,
    adapter: &NixAdapter,
    findings: &mut Vec<Diagnostic>,
) -> Result<(), DoctorCacheError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;

    if let Ok(status) = discovery_cache_status() {
        if status.path.is_empty() {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "cache.discovery.unavailable",
                "discovery cache unavailable on this host".to_owned(),
            );
        } else {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "cache.discovery.status",
                format!(
                    "discovery cache: {} ({} entr{}, {} bytes)",
                    status.path,
                    status.entries,
                    if status.entries == 1 { "y" } else { "ies" },
                    status.total_bytes
                ),
            );
        }
    }

    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };

    if let Ok(entry) = discovery_cache_entry(&context) {
        if !entry.available {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "cache.discovery.remote",
                "discovery cache disabled for remote flakes".to_owned(),
            );
        } else {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                if entry.hit {
                    "cache.discovery.hit"
                } else {
                    "cache.discovery.miss"
                },
                if entry.hit {
                    "discovery cache hit for current flake inputs".to_owned()
                } else {
                    "discovery cache miss for current flake inputs".to_owned()
                },
            );
        }

        if let Some(key) = entry.invalidation_key {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "cache.discovery.invalidation_key",
                format!("nix tree invalidation key: {key}"),
            );
        }

        if let Some(file) = entry.cache_file {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "cache.discovery.file",
                format!("discovery cache file: {file}"),
            );
        }
    }

    if adapter.capability_provenance.from_cache {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.capability.hit",
            "capability cache hit".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.capability.miss",
            "capability cache miss".to_owned(),
        );
    }

    Ok(())
}

fn collect_capability_cache_findings(findings: &mut Vec<Diagnostic>) {
    if let Ok(status) = capability_cache_status() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "cache.capability.status",
            format!(
                "capability cache: {} ({} entr{}, {} bytes)",
                status.path,
                status.entries,
                if status.entries == 1 { "y" } else { "ies" },
                status.total_bytes
            ),
        );
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

fn write_human_report(writer: &mut impl Write, report: &DoctorCacheReport) -> io::Result<()> {
    if report.findings.is_empty() {
        writeln!(writer, "doctor cache: no findings")?;
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

fn write_json_report(writer: &mut impl Write, report: &DoctorCacheReport) -> io::Result<()> {
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(writer, "{rendered}")?;
    Ok(())
}
