//! Determinate Nix integration probes for doctor diagnostics.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::capabilities::{
    NixDistribution, config_bool_setting, parse_nix_distribution, probe_config_json,
};

/// Whether lazy trees are configured and enabled when readable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LazyTreesState {
    /// `lazy-trees` is not present in Nix config.
    Unconfigured,
    Enabled,
    Disabled,
}

/// Performance-related Determinate features inferred from distribution and config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeterminatePerformanceFeatures {
    pub parallel_eval_available: bool,
    pub lazy_trees: LazyTreesState,
}

/// Read-only `determinate-nixd` probe summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NixdProbe {
    pub executable: Utf8PathBuf,
    pub version: Option<String>,
    pub status_summary: Option<String>,
}

/// Redact sensitive values from diagnostic text before JSON emission.
#[must_use]
pub fn redact_sensitive_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for line in input.lines() {
        if let Some(redacted) = redact_sensitive_line(line) {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&redacted);
        }
    }
    output
}

fn redact_sensitive_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("apikey")
        || lower.contains("api_key")
        || lower.contains("bearer")
    {
        return Some(redact_key_value_line(line));
    }

    if line.chars().filter(|ch| ch.is_ascii_hexdigit()).count() >= 32
        && line.contains(':')
        && looks_like_credential_assignment(line)
    {
        return Some(redact_key_value_line(line));
    }

    Some(line.to_owned())
}

fn looks_like_credential_assignment(line: &str) -> bool {
    line.split_once([':', '=']).is_some_and(|(key, _)| {
        let key = key.trim().to_ascii_lowercase();
        key.contains("token")
            || key.contains("secret")
            || key.contains("password")
            || key.contains("auth")
    })
}

fn redact_key_value_line(line: &str) -> String {
    if let Some((key, _)) = line.split_once(':') {
        return format!("{}: [redacted]", key.trim());
    }
    if let Some((key, _)) = line.split_once('=') {
        return format!("{}=[redacted]", key.trim());
    }
    "[redacted]".to_owned()
}

/// Infer Determinate performance features from distribution and Nix config.
#[must_use]
pub fn probe_performance_features(
    distribution: &NixDistribution,
    config_json: Option<&str>,
) -> DeterminatePerformanceFeatures {
    let parallel_eval_available = distribution.is_determinate();
    let lazy_trees = match config_bool_setting(config_json, "lazy-trees") {
        Some(true) => LazyTreesState::Enabled,
        Some(false) => LazyTreesState::Disabled,
        None => LazyTreesState::Unconfigured,
    };

    DeterminatePerformanceFeatures {
        parallel_eval_available,
        lazy_trees,
    }
}

/// Probe `determinate-nixd` with read-only commands when present.
#[must_use]
pub fn probe_nixd() -> Option<NixdProbe> {
    let executable = which::which("determinate-nixd")
        .ok()
        .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())?;

    let version = run_nixd_command(&executable, &["version"]);
    let status_summary =
        run_nixd_command(&executable, &["status"]).map(|text| redact_sensitive_text(&text));

    Some(NixdProbe {
        executable,
        version,
        status_summary,
    })
}

fn run_nixd_command(executable: &Utf8Path, args: &[&str]) -> Option<String> {
    let output = Command::new(executable.as_std_path())
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Whether the host looks like macOS from a Nix system string.
#[must_use]
pub fn host_is_macos(system: &str) -> bool {
    system.contains("darwin")
}

/// Parse distribution and load config for doctor probes.
#[must_use]
pub fn probe_distribution_context(
    nix: &Utf8Path,
    version_banner: &str,
) -> (NixDistribution, Option<String>) {
    let distribution = parse_nix_distribution(version_banner);
    let config_json = probe_config_json(nix);
    (distribution, config_json)
}

#[cfg(test)]
mod tests {
    use super::{
        LazyTreesState, NixDistribution, host_is_macos, probe_performance_features,
        redact_sensitive_text,
    };

    #[test]
    fn redact_sensitive_text_strips_token_lines() {
        let input = "logged in: yes\ntoken: super-secret-value\nversion: 1.2.3";
        let redacted = redact_sensitive_text(input);
        assert!(redacted.contains("logged in: yes"));
        assert!(redacted.contains("token: [redacted]"));
        assert!(!redacted.contains("super-secret-value"));
        assert!(redacted.contains("version: 1.2.3"));
    }

    #[test]
    fn redact_sensitive_text_strips_bearer_and_api_key_assignments() {
        let input = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz\napi_key=deadbeefdeadbeef";
        let redacted = redact_sensitive_text(input);
        assert!(redacted.contains("Authorization: [redacted]"));
        assert!(redacted.contains("api_key=[redacted]"));
        assert!(!redacted.contains("deadbeef"));
    }

    #[test]
    fn upstream_distribution_reports_performance_not_applicable() {
        let features = probe_performance_features(&NixDistribution::Upstream, None);
        assert!(!features.parallel_eval_available);
        assert_eq!(features.lazy_trees, LazyTreesState::Unconfigured);
    }

    #[test]
    fn determinate_distribution_reports_parallel_eval_available() {
        let config = r#"{"lazy-trees": {"value": false}}"#;
        let features = probe_performance_features(
            &NixDistribution::Determinate {
                product_version: Some("3.21.7".to_owned()),
            },
            Some(config),
        );
        assert!(features.parallel_eval_available);
        assert_eq!(features.lazy_trees, LazyTreesState::Disabled);
    }

    #[test]
    fn host_is_macos_detects_darwin_systems() {
        assert!(host_is_macos("aarch64-darwin"));
        assert!(!host_is_macos("x86_64-linux"));
    }
}
