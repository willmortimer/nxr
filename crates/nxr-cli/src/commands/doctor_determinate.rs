//! `nxr doctor determinate` read-only Determinate Nix diagnostics.

use std::io::{self, Write};

use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel, exit};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{
    LazyTreesState, NixAdapter, NixDistribution, NixError, OptionalNixFlags,
    config_string_list_setting, distribution_from_version_banner, effective_experimental_features,
    host_is_macos, probe_ci_environment, probe_nixd, probe_performance_features,
    probe_wasm_support, redact_sensitive_text,
};
use serde::Serialize;

use crate::commands::common::WorkspaceState;
use crate::runner_output::RunnerOutput;

const SCHEMA_VERSION: u32 = 1;

/// Errors while running determinate doctor output.
#[derive(Debug, thiserror::Error)]
pub enum DoctorDeterminateError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DoctorDeterminateError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned determinate doctor report envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorDeterminateReport {
    pub schema_version: u32,
    pub distribution: String,
    pub findings: Vec<Diagnostic>,
}

/// Inputs for determinate doctor diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoctorDeterminateRequest<'a> {
    pub nix_override: Option<&'a str>,
    pub all: bool,
    pub refresh: bool,
}

/// Run Determinate-specific read-only diagnostics.
///
/// # Errors
///
/// Returns [`DoctorDeterminateError`] when writing output fails.
pub fn run(
    request: DoctorDeterminateRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DoctorDeterminateError> {
    let mut findings = Vec::new();
    let nix_flags = OptionalNixFlags::default();
    let mut workspace = WorkspaceState::new(None, request.nix_override, &nix_flags);
    let distribution = collect_findings(request, &mut findings, &mut workspace);

    runner
        .info("running determinate doctor diagnostics")
        .map_err(DoctorDeterminateError::Io)?;

    let report = DoctorDeterminateReport {
        schema_version: SCHEMA_VERSION,
        distribution: distribution.label().to_owned(),
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
    request: DoctorDeterminateRequest<'_>,
    findings: &mut Vec<Diagnostic>,
    workspace: &mut WorkspaceState<'_>,
) -> NixDistribution {
    let adapter = match workspace.adapter_refresh(request.refresh) {
        Ok(adapter) => adapter,
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "determinate.nix.missing",
                nix_missing_message(&error),
            );
            return NixDistribution::Unknown;
        }
    };

    let distribution = distribution_from_version_banner(&adapter.version_banner);

    if adapter.capability_provenance.from_cache {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.capability_cache.hit",
            "capability cache hit".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.capability_cache.miss",
            "capability cache miss".to_owned(),
        );
    }

    match &distribution {
        NixDistribution::Determinate { product_version } => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.distribution.detected",
                format!(
                    "Determinate Nix detected (product {}, compatibility {})",
                    product_version.as_deref().unwrap_or("unknown"),
                    adapter.capabilities.version
                ),
            );
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.nix.executable",
                format!("nix executable: {}", adapter.nix),
            );
            collect_determinate_feature_findings(
                &adapter,
                &distribution,
                adapter.config_json.as_deref(),
                findings,
            );
            collect_config_findings(adapter.config_json.as_deref(), findings);
            collect_ci_findings(findings);
            let nixd_probe = probe_nixd();
            collect_nixd_findings(&nixd_probe, findings);
            if request.all {
                collect_builder_findings(
                    &adapter,
                    adapter.config_json.as_deref(),
                    nixd_probe.is_some(),
                    findings,
                );
            }
        }
        NixDistribution::Upstream | NixDistribution::Lix => {
            let label = match distribution {
                NixDistribution::Upstream => "upstream Nix",
                NixDistribution::Lix => "Lix",
                _ => unreachable!("matched upstream or lix"),
            };
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.distribution.na",
                format!("Determinate diagnostics not applicable ({label})"),
            );
        }
        NixDistribution::Unknown => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.distribution.unknown",
                "could not classify Nix distribution from version banner".to_owned(),
            );
        }
    }

    distribution
}

fn collect_determinate_feature_findings(
    adapter: &NixAdapter,
    distribution: &NixDistribution,
    config_json: Option<&str>,
    findings: &mut Vec<Diagnostic>,
) {
    let features = probe_performance_features(distribution, config_json);

    if features.parallel_eval_available {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.parallel_eval.available",
            "parallel evaluation available on Determinate Nix".to_owned(),
        );
    }

    match features.lazy_trees {
        LazyTreesState::Enabled => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.lazy_trees.enabled",
                "lazy trees enabled".to_owned(),
            );
        }
        LazyTreesState::Disabled => {
            push_finding(
                findings,
                DiagnosticLevel::Warning,
                "determinate.lazy_trees.disabled",
                "lazy trees explicitly disabled in Nix configuration".to_owned(),
            );
        }
        LazyTreesState::Unconfigured => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.lazy_trees.unconfigured",
                "lazy trees not explicitly configured".to_owned(),
            );
        }
    }

    if adapter.capabilities.flakes_enabled {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.flakes.enabled",
            format!(
                "flakes enabled (compatibility version {})",
                adapter.capabilities.version
            ),
        );
    }
}

fn collect_config_findings(config_json: Option<&str>, findings: &mut Vec<Diagnostic>) {
    if let Some(features) = effective_experimental_features(config_json) {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.experimental_features.effective",
            format!(
                "effective experimental features ({}): {}",
                features.len(),
                features.join(", ")
            ),
        );
    } else if config_json.is_some() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.experimental_features.unconfigured",
            "no experimental features configured".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.experimental_features.unavailable",
            "could not read experimental features from Nix configuration".to_owned(),
        );
    }

    if let Some(substituters) = config_string_list_setting(config_json, "substituters") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.substituters.configured",
            format!(
                "Nix substituters ({}): {}",
                substituters.len(),
                redact_sensitive_text(&substituters.join(" "))
            ),
        );
    } else if config_json.is_some() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.substituters.unconfigured",
            "no substituters configured".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.substituters.unavailable",
            "could not read substituters from Nix configuration".to_owned(),
        );
    }

    if let Some(keys) = config_string_list_setting(config_json, "trusted-public-keys") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.trusted_keys.configured",
            format!("trusted public keys: {}", keys.len()),
        );
    }

    if let Some(keys) = config_string_list_setting(config_json, "extra-substituters") {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.extra_substituters.configured",
            format!(
                "extra substituters ({}): {}",
                keys.len(),
                redact_sensitive_text(&keys.join(" "))
            ),
        );
    }

    let wasm = probe_wasm_support(config_json);
    if wasm.wasm_builtin {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.wasm_builtin.enabled",
            "wasm-builtin experimental feature enabled".to_owned(),
        );
    } else if config_json.is_some() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.wasm_builtin.unconfigured",
            "wasm-builtin experimental feature not enabled".to_owned(),
        );
    }

    if wasm.wasm_derivations {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.wasm_derivations.enabled",
            "wasm-derivations experimental feature enabled".to_owned(),
        );
    } else if config_json.is_some() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.wasm_derivations.unconfigured",
            "wasm-derivations experimental feature not enabled".to_owned(),
        );
    }
}

fn collect_ci_findings(findings: &mut Vec<Diagnostic>) {
    if let Some(label) = probe_ci_environment() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.ci.detected",
            format!("CI environment detected ({label})"),
        );
    }
}

fn collect_nixd_findings(nixd_probe: &Option<nxr_nix::NixdProbe>, findings: &mut Vec<Diagnostic>) {
    match nixd_probe {
        Some(probe) => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.nixd.found",
                format!("determinate-nixd found at {}", probe.executable),
            );
            if let Some(version) = &probe.version {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "determinate.nixd.version",
                    format!(
                        "determinate-nixd version: {}",
                        redact_sensitive_text(version)
                    ),
                );
            }
            if let Some(status) = &probe.status_summary {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "determinate.nixd.status",
                    redact_sensitive_text(status),
                );
            }
        }
        None => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.nixd.missing",
                "determinate-nixd not found on PATH".to_owned(),
            );
        }
    }
}

fn collect_builder_findings(
    adapter: &NixAdapter,
    config_json: Option<&str>,
    nixd_present: bool,
    findings: &mut Vec<Diagnostic>,
) {
    if let Some(builders) = config_string_list_setting(config_json, "builders") {
        if builders.is_empty() {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.builders.configured.empty",
                "no remote builders configured".to_owned(),
            );
        } else {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "determinate.builders.configured",
                format!(
                    "remote builders configured ({}): {}",
                    builders.len(),
                    redact_sensitive_text(&builders.join("; "))
                ),
            );
            if nixd_present {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "determinate.builders.reachability.nixd",
                    "remote builders configured and determinate-nixd is present \
                     (reachability not probed)"
                        .to_owned(),
                );
            } else {
                push_finding(
                    findings,
                    DiagnosticLevel::Warning,
                    "determinate.builders.reachability.nixd_missing",
                    "remote builders configured but determinate-nixd is not on PATH \
                     (reachability not probed)"
                        .to_owned(),
                );
            }
        }
    } else if config_json.is_some() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.builders.configured.empty",
            "no remote builders configured".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.builders.configured.unavailable",
            "could not read builders from Nix configuration".to_owned(),
        );
    }

    if host_is_macos(&adapter.system) {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.builder.macos",
            "macOS host: native Linux builder may be configured via Determinate Nixd \
             (one CPU is often faster with the Virtualization framework)"
                .to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "determinate.builder.linux",
            "Linux host: use remote builders or Determinate CI for cross-platform builds"
                .to_owned(),
        );
    }
}

fn nix_missing_message(error: &NixError) -> String {
    match error {
        NixError::NixNotFound { path } => format!("nix not found at {path}"),
        other => other.user_message(),
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

fn write_human_report(writer: &mut impl Write, report: &DoctorDeterminateReport) -> io::Result<()> {
    if report.findings.is_empty() {
        writeln!(writer, "doctor determinate: no findings")?;
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

fn write_json_report(writer: &mut impl Write, report: &DoctorDeterminateReport) -> io::Result<()> {
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(writer, "{rendered}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DoctorDeterminateReport, write_json_report};
    use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel};
    use nxr_nix::{NixAdapter, NixCapabilities, NixVersion, redact_sensitive_text};

    fn test_adapter() -> NixAdapter {
        NixAdapter::with_parts(
            camino::Utf8PathBuf::from("/nix/var/nix/profiles/default/bin/nix"),
            "aarch64-darwin".to_owned(),
            NixCapabilities::all_supported_for_tests(NixVersion::new(2, 34, 8)),
        )
    }

    fn test_config_json() -> String {
        r#"{
            "lazy-trees": {"value": false},
            "experimental-features": {"value": ["nix-command", "flakes", "wasm-builtin"]},
            "substituters": {"value": ["https://cache.nixos.org"]},
            "trusted-public-keys": {"value": ["cache.nixos.org-1:abc"]}
        }"#
        .to_owned()
    }

    #[test]
    fn upstream_na_finding_serializes_without_secrets() {
        let report = DoctorDeterminateReport {
            schema_version: 1,
            distribution: "upstream".to_owned(),
            findings: vec![Diagnostic {
                level: DiagnosticLevel::Info,
                code: "determinate.distribution.na".to_owned(),
                message: "Determinate diagnostics not applicable (upstream Nix)".to_owned(),
            }],
        };
        let mut output = Vec::new();
        write_json_report(&mut output, &report).expect("write json");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("determinate.distribution.na"));
        assert!(!rendered.contains("token"));
    }

    #[test]
    fn json_findings_redact_sensitive_nixd_status() {
        let secret = "token: super-secret-value";
        let redacted = redact_sensitive_text(secret);
        let report = DoctorDeterminateReport {
            schema_version: 1,
            distribution: "determinate".to_owned(),
            findings: vec![Diagnostic {
                level: DiagnosticLevel::Info,
                code: "determinate.nixd.status".to_owned(),
                message: redacted,
            }],
        };
        let mut output = Vec::new();
        write_json_report(&mut output, &report).expect("write json");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("super-secret-value"));
    }

    #[test]
    fn determinate_feature_findings_include_lazy_trees_warning() {
        let adapter = test_adapter();
        let config = r#"{"lazy-trees": {"value": false}}"#;
        let mut findings = Vec::new();
        super::collect_determinate_feature_findings(
            &adapter,
            &nxr_nix::NixDistribution::Determinate {
                product_version: Some("3.21.7".to_owned()),
            },
            Some(config),
            &mut findings,
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "determinate.lazy_trees.disabled")
        );
    }

    #[test]
    fn config_findings_include_experimental_features_and_substituters() {
        let mut findings = Vec::new();
        super::collect_config_findings(Some(&test_config_json()), &mut findings);
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "determinate.experimental_features.effective")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "determinate.substituters.configured")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "determinate.trusted_keys.configured")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "determinate.wasm_builtin.enabled")
        );
    }

    #[test]
    fn push_finding_uses_stable_codes() {
        let mut findings = Vec::new();
        super::push_finding(
            &mut findings,
            DiagnosticLevel::Info,
            "determinate.distribution.na",
            "test".to_owned(),
        );
        assert_eq!(findings[0].code, "determinate.distribution.na");
    }
}
