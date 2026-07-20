//! `nxr doctor` static and clean-environment diagnostics.

use std::io::{self, Write};

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel, exit};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{NixAdapter, NixCapabilities, NixError, OptionalNixFlags, resolve_app_by_name};
use serde::Serialize;

use crate::commands::common::{
    AppRequest, PrepareError, build_adapter, current_invocation_directory, prepare_app_plan,
};
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

const SCHEMA_VERSION: u32 = 1;

/// Heuristic PATH segments that often indicate development-shell pollution.
const PATH_POLLUTION_MARKERS: &[&str] = &[
    "/node_modules/.bin",
    "/.cargo/bin",
    "/.local/share/mise",
    "/.mise/",
    "/mise/shims",
    "/.asdf/",
    "/.nix-profile/",
    "/nix/var/nix/profiles/",
    "/opt/homebrew/",
    "/.rbenv/",
    "/.pyenv/",
    "/.volta/",
    "/.fnm/",
];

/// Errors while running doctor output.
#[derive(Debug, thiserror::Error)]
pub enum DoctorError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DoctorError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned doctor report envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<NixCapabilities>,
    pub findings: Vec<Diagnostic>,
}

/// Inputs for doctor diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoctorRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub app: Option<&'a str>,
    pub clean_env: bool,
    pub all: bool,
    pub root: bool,
    pub cwd: Option<&'a str>,
}

/// Run static (and optional clean-environment) diagnostics.
///
/// Doctor never executes apps. With `--clean-env` and a named app it may emit a
/// dry-run plan only.
///
/// # Errors
///
/// Returns [`DoctorError`] when writing output fails.
pub fn run(
    request: DoctorRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DoctorError> {
    let mut findings = Vec::new();
    let capabilities = collect_findings(request, &mut findings);
    let exit_code = exit_code_for_findings(&findings);

    runner
        .info("running doctor diagnostics")
        .map_err(DoctorError::Io)?;

    let report = DoctorReport {
        schema_version: SCHEMA_VERSION,
        capabilities,
        findings,
    };
    let mut stdout = io::stdout().lock();
    if json {
        write_json_report(&mut stdout, &report)?;
    } else {
        write_human_report(&mut stdout, &report)?;
    }

    Ok(exit_code)
}

fn collect_findings(
    request: DoctorRequest<'_>,
    findings: &mut Vec<Diagnostic>,
) -> Option<NixCapabilities> {
    let adapter = match build_adapter(request.nix_override) {
        Ok(adapter) => {
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "nix.found",
                format!("nix found at {}", adapter.nix),
            );
            Some(adapter)
        }
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "nix.missing",
                nix_missing_message(&error),
            );
            None
        }
    };

    let capabilities = adapter.as_ref().map(|adapter| adapter.capabilities.clone());

    if let Some(adapter) = adapter.as_ref() {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "system.detected",
            format!("system detected: {}", adapter.system),
        );
        push_capability_findings(adapter, findings);
        if adapter.capabilities.flakes_enabled {
            collect_flake_findings(request, adapter, findings);
        }
    }

    if request.clean_env {
        collect_clean_env_findings(request, findings);
    }

    capabilities
}

fn push_capability_findings(adapter: &NixAdapter, findings: &mut Vec<Diagnostic>) {
    let caps = &adapter.capabilities;
    push_finding(
        findings,
        DiagnosticLevel::Info,
        "nix.version",
        format!("nix version {}", caps.version),
    );

    if caps.flakes_enabled {
        push_finding(
            findings,
            DiagnosticLevel::Info,
            "nix.flakes_enabled",
            "flakes enabled".to_owned(),
        );
    } else {
        push_finding(
            findings,
            DiagnosticLevel::Error,
            "nix.flakes_disabled",
            NixError::FlakesDisabled {
                version: caps.version,
            }
            .user_message(),
        );
    }
}

fn collect_flake_findings(
    request: DoctorRequest<'_>,
    adapter: &NixAdapter,
    findings: &mut Vec<Diagnostic>,
) {
    let invocation_cwd = match current_invocation_directory() {
        Ok(cwd) => cwd,
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "flake.missing",
                error.to_string(),
            );
            return;
        }
    };

    let flake = match resolve_flake(request.flake_arg, &invocation_cwd) {
        Ok(flake) => flake,
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "flake.missing",
                flake_error_message(&error),
            );
            return;
        }
    };

    push_finding(
        findings,
        DiagnosticLevel::Info,
        "flake.discovered",
        format!("flake discovered: {}", flake.display),
    );

    match adapter.discover_apps(&flake.nix_ref, &OptionalNixFlags::default()) {
        Ok(apps) => {
            if apps.is_empty() {
                push_finding(
                    findings,
                    DiagnosticLevel::Warning,
                    "apps.empty",
                    format!("no apps found for system {}", adapter.system),
                );
            } else {
                push_finding(
                    findings,
                    DiagnosticLevel::Info,
                    "apps.listed",
                    format!("listed {} app(s) for system {}", apps.len(), adapter.system),
                );
            }

            if request.all {
                collect_app_quality_findings(&apps, findings);
            }

            if let Some(app_name) = request.app {
                match resolve_app_by_name(&apps, app_name) {
                    Ok(app) => {
                        push_finding(
                            findings,
                            DiagnosticLevel::Info,
                            "app.found",
                            format!("app found: {}", app.name),
                        );
                    }
                    Err(error) => {
                        push_finding(
                            findings,
                            DiagnosticLevel::Error,
                            "app.missing",
                            error.to_string(),
                        );
                    }
                }
            }
        }
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "apps.unavailable",
                error.user_message(),
            );
        }
    }
}

fn collect_app_quality_findings(apps: &[nxr_core::App], findings: &mut Vec<Diagnostic>) {
    for app in apps {
        match app.description.as_deref() {
            None | Some("") => {
                push_finding(
                    findings,
                    DiagnosticLevel::Warning,
                    "app.description_missing",
                    format!("app `{}` has no description", app.name),
                );
            }
            Some(_) => {}
        }

        if !is_recommended_app_name(&app.name) {
            push_finding(
                findings,
                DiagnosticLevel::Warning,
                "app.naming",
                format!(
                    "app `{}` does not follow recommended naming \
                     (lowercase alphanumeric with single `-` separators)",
                    app.name
                ),
            );
        }
    }
}

fn is_recommended_app_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return false;
    }
    name.chars()
        .all(|ch| matches!(ch, 'a'..='z' | '0'..='9' | '-'))
}

fn collect_clean_env_findings(request: DoctorRequest<'_>, findings: &mut Vec<Diagnostic>) {
    push_finding(
        findings,
        DiagnosticLevel::Info,
        "clean_env.policy",
        "clean mode uses a documented allowlist (for example HOME, USER, TMPDIR, \
         XDG_RUNTIME_DIR) plus explicit --keep-env / --set-env overrides; apps are \
         not executed in doctor mode"
            .to_owned(),
    );

    for segment in path_pollution_segments() {
        push_finding(
            findings,
            DiagnosticLevel::Warning,
            "path.polluted",
            format!("PATH contains development tooling: {segment}"),
        );
    }

    let Some(app_name) = request.app else {
        return;
    };

    let nix_flags = OptionalNixFlags::default();
    let app_request = AppRequest {
        flake_arg: request.flake_arg,
        nix_override: request.nix_override,
        app: app_name,
        args: &[],
        root: request.root,
        cwd: request.cwd,
        shell: None,
        environment_policy: EnvironmentPolicy::Inherit,
        nix_flags: &nix_flags,
    };

    match prepare_app_plan(&app_request) {
        Ok(prepared) => {
            let command = prepared.plan.command.arguments.join(" ");
            push_finding(
                findings,
                DiagnosticLevel::Info,
                "plan.available",
                format!(
                    "dry-run plan for {}: {} {}",
                    prepared.plan.target, prepared.plan.command.program, command
                ),
            );
        }
        Err(error) => {
            push_finding(
                findings,
                DiagnosticLevel::Error,
                "plan.unavailable",
                prepare_error_message(&error),
            );
        }
    }
}

fn path_pollution_segments() -> Vec<String> {
    let Ok(path) = std::env::var("PATH") else {
        return Vec::new();
    };

    path.split(':')
        .filter(|segment| !segment.is_empty())
        .filter(|segment| path_segment_is_polluted(segment))
        .map(ToOwned::to_owned)
        .collect()
}

fn path_segment_is_polluted(segment: &str) -> bool {
    PATH_POLLUTION_MARKERS
        .iter()
        .any(|marker| segment.contains(marker))
}

fn nix_missing_message(error: &NixError) -> String {
    match error {
        NixError::NixNotFound { path } => format!("nix not found at {path}"),
        other => other.user_message(),
    }
}

fn flake_error_message(error: &FlakeResolveError) -> String {
    error.to_string()
}

fn prepare_error_message(error: &PrepareError) -> String {
    error.to_string()
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

fn write_human_report(writer: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
    if report.findings.is_empty() {
        writeln!(writer, "doctor: no findings")?;
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

fn write_json_report(writer: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
    let rendered = serde_json::to_string_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(writer, "{rendered}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DoctorReport, exit_code_for_findings, path_segment_is_polluted, write_human_report,
        write_json_report,
    };
    use nxr_core::diagnostics::{Diagnostic, DiagnosticLevel};

    #[test]
    fn path_pollution_markers_match_common_dev_paths() {
        assert!(path_segment_is_polluted(
            "/Users/dev/project/node_modules/.bin"
        ));
        assert!(path_segment_is_polluted("/Users/dev/.cargo/bin"));
        assert!(!path_segment_is_polluted("/usr/bin"));
    }

    #[test]
    fn exit_code_is_nonzero_when_errors_present() {
        let findings = vec![
            Diagnostic {
                level: DiagnosticLevel::Info,
                code: "nix.found".to_owned(),
                message: "ok".to_owned(),
            },
            Diagnostic {
                level: DiagnosticLevel::Error,
                code: "app.missing".to_owned(),
                message: "missing".to_owned(),
            },
        ];
        assert_eq!(exit_code_for_findings(&findings), 1);
    }

    #[test]
    fn exit_code_is_zero_without_errors() {
        let findings = vec![Diagnostic {
            level: DiagnosticLevel::Warning,
            code: "path.polluted".to_owned(),
            message: "warn".to_owned(),
        }];
        assert_eq!(exit_code_for_findings(&findings), 0);
    }

    #[test]
    fn human_report_prints_level_code_and_message() {
        let report = DoctorReport {
            schema_version: 1,
            capabilities: None,
            findings: vec![Diagnostic {
                level: DiagnosticLevel::Info,
                code: "flake.discovered".to_owned(),
                message: "flake discovered: .".to_owned(),
            }],
        };
        let mut output = Vec::new();
        write_human_report(&mut output, &report).expect("write human report");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("info: flake.discovered: flake discovered: .\n"));
    }

    #[test]
    fn json_report_includes_schema_version_findings_and_capabilities() {
        let report = DoctorReport {
            schema_version: 1,
            capabilities: Some(nxr_nix::NixCapabilities::all_supported_for_tests(
                nxr_nix::NixVersion::new(2, 18, 1),
            )),
            findings: vec![Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "path.polluted".to_owned(),
                message: "PATH contains development tooling: /tmp/.cargo/bin".to_owned(),
            }],
        };
        let mut output = Vec::new();
        write_json_report(&mut output, &report).expect("write json report");
        let value: serde_json::Value = serde_json::from_slice(&output).expect("parse doctor json");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["findings"][0]["level"], "warning");
        assert_eq!(value["findings"][0]["code"], "path.polluted");
        assert_eq!(value["capabilities"]["version"], "2.18.1");
        assert_eq!(value["capabilities"]["flakes_enabled"], true);
        assert_eq!(value["capabilities"]["supports_offline"], true);
        assert_eq!(value["capabilities"]["supports_json_log_format"], true);
        assert_eq!(value["capabilities"]["supports_no_write_lock_file"], true);
        assert_eq!(value["capabilities"]["supports_accept_flake_config"], true);
    }
}
