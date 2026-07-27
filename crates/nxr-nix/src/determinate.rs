//! Determinate Nix integration probes for doctor diagnostics.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::capabilities::{
    NixDistribution, config_bool_setting, config_string_list_setting, parse_nix_distribution,
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

/// WebAssembly-related experimental features from effective Nix configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeterminateWasmSupport {
    pub wasm_builtin: bool,
    pub wasm_derivations: bool,
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
        let redacted = redact_sensitive_line(line);
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&redacted);
    }
    output
}

fn redact_sensitive_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("apikey")
        || lower.contains("api_key")
        || lower.contains("bearer")
    {
        return redact_key_value_line(line);
    }

    if line.chars().filter(char::is_ascii_hexdigit).count() >= 32
        && line.contains(':')
        && looks_like_credential_assignment(line)
    {
        return redact_key_value_line(line);
    }

    line.to_owned()
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

/// Effective experimental features from `nix config show --json` output.
#[must_use]
pub fn effective_experimental_features(config_json: Option<&str>) -> Option<Vec<String>> {
    let mut features =
        config_string_list_setting(config_json, "experimental-features").unwrap_or_default();
    if let Some(extra) = config_string_list_setting(config_json, "extra-experimental-features") {
        features.extend(extra);
    }
    if features.is_empty() {
        None
    } else {
        Some(features)
    }
}

/// Detect WebAssembly experimental features when present in effective config.
#[must_use]
pub fn probe_wasm_support(config_json: Option<&str>) -> DeterminateWasmSupport {
    let features = effective_experimental_features(config_json).unwrap_or_default();
    DeterminateWasmSupport {
        wasm_builtin: features.iter().any(|feature| feature == "wasm-builtin"),
        wasm_derivations: features.iter().any(|feature| feature == "wasm-derivations"),
    }
}

/// Detect a CI environment label from well-known environment variables.
#[must_use]
pub fn probe_ci_environment() -> Option<&'static str> {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        return Some("github-actions");
    }
    if std::env::var_os("GITLAB_CI").is_some() {
        return Some("gitlab-ci");
    }
    if std::env::var_os("BUILDKITE").is_some() {
        return Some("buildkite");
    }
    if std::env::var_os("CIRCLECI").is_some() {
        return Some("circleci");
    }
    if std::env::var_os("CI").is_some() {
        return Some("generic-ci");
    }
    None
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

/// Parse distribution from a captured `nix --version` banner.
#[must_use]
pub fn distribution_from_version_banner(version_banner: &str) -> NixDistribution {
    parse_nix_distribution(version_banner)
}

#[cfg(test)]
mod tests {
    use super::{
        DeterminateWasmSupport, LazyTreesState, NixDistribution, effective_experimental_features,
        host_is_macos, probe_ci_environment, probe_performance_features, probe_wasm_support,
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

    #[test]
    fn effective_experimental_features_merges_extra_features() {
        let config = r#"{
            "experimental-features": {"value": ["nix-command", "flakes"]},
            "extra-experimental-features": {"value": ["wasm-builtin"]}
        }"#;
        let features = effective_experimental_features(Some(config)).expect("features");
        assert!(features.contains(&"nix-command".to_owned()));
        assert!(features.contains(&"wasm-builtin".to_owned()));
    }

    #[test]
    fn probe_wasm_support_reads_experimental_features() {
        let config = r#"{"experimental-features": {"value": ["wasm-derivations"]}}"#;
        let support = probe_wasm_support(Some(config));
        assert_eq!(
            support,
            DeterminateWasmSupport {
                wasm_builtin: false,
                wasm_derivations: true,
            }
        );
    }

    #[test]
    fn probe_ci_environment_without_ci_vars_is_none_when_unset() {
        if std::env::var_os("GITHUB_ACTIONS").is_some()
            || std::env::var_os("GITLAB_CI").is_some()
            || std::env::var_os("BUILDKITE").is_some()
            || std::env::var_os("CIRCLECI").is_some()
            || std::env::var_os("CI").is_some()
        {
            return;
        }
        assert_eq!(probe_ci_environment(), None);
    }
}
