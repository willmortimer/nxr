//! Nix executable discovery, version parsing, and capability negotiation.

use std::fmt;
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::NixError;
use crate::command::{self, NIX_EXECUTABLE_ENV};

/// Lowest Nix release exercised in CI / documented as the tested support floor.
///
/// Capability negotiation still runs on older releases; this constant documents
/// what nxr actively validates (see `docs/COMPATIBILITY.md`).
pub const TESTED_NIX_SUPPORT_FLOOR: NixVersion = NixVersion {
    major: 2,
    minor: 18,
    patch: 0,
};

/// Features generally available from this floor onward (used when help/config
/// probing is unavailable).
const FEATURE_FLOOR: NixVersion = NixVersion {
    major: 2,
    minor: 4,
    patch: 0,
};

/// Whether a Nix failure should map to capability (4) vs evaluation (5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NixFailureKind {
    Capability,
    Evaluation,
}

/// Parsed Nix CLI version (`major.minor.patch`).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NixVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl NixVersion {
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for NixVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for NixVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Negotiated Nix CLI capabilities for the current host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // capability bitfield matches the negotiated feature set
pub struct NixCapabilities {
    pub version: NixVersion,
    pub flakes_enabled: bool,
    pub supports_json_log_format: bool,
    pub supports_no_write_lock_file: bool,
    pub supports_offline: bool,
    pub supports_accept_flake_config: bool,
}

/// Optional Nix global flags a caller may request; unsupported ones are dropped.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // mirrors NixCapabilities optional flag surface
pub struct OptionalNixFlags {
    pub offline: bool,
    pub no_write_lock_file: bool,
    pub accept_flake_config: bool,
    pub json_log_format: bool,
    /// `--option KEY VALUE` pairs from `--nix-option KEY=VAL`.
    pub nix_options: Vec<(String, String)>,
    /// Raw argv fragments from repeatable `--nix-arg` (passed through when set).
    pub extra_argv: Vec<String>,
}

impl NixCapabilities {
    /// Fully-capable fixture for unit tests that do not probe a real Nix.
    #[must_use]
    pub const fn all_supported_for_tests(version: NixVersion) -> Self {
        Self {
            version,
            flakes_enabled: true,
            supports_json_log_format: true,
            supports_no_write_lock_file: true,
            supports_offline: true,
            supports_accept_flake_config: true,
        }
    }

    /// Global flags that are both requested and supported (true Nix globals).
    #[must_use]
    pub fn select_compatible_globals(&self, requested: &OptionalNixFlags) -> Vec<String> {
        let mut flags = Vec::new();
        if requested.json_log_format && self.supports_json_log_format {
            flags.push("--log-format".to_owned());
            flags.push("json".to_owned());
        }
        if requested.offline && self.supports_offline {
            flags.push("--offline".to_owned());
        }
        if requested.accept_flake_config && self.supports_accept_flake_config {
            flags.push("--accept-flake-config".to_owned());
        }
        for (key, value) in &requested.nix_options {
            flags.push("--option".to_owned());
            flags.push(key.clone());
            flags.push(value.clone());
        }
        flags.extend(requested.extra_argv.iter().cloned());
        flags
    }

    /// Build a compatible argv: true globals first, then the command, then
    /// installable-scoped options such as `--no-write-lock-file`.
    #[must_use]
    pub fn apply_optional_flags(
        &self,
        base_args: Vec<String>,
        requested: &OptionalNixFlags,
    ) -> Vec<String> {
        let mut out = self.select_compatible_globals(requested);
        let mut rest = base_args;

        // Peel the Nix verb (and `flake <subcommand>`) so lock-file options sit
        // after the command words — Nix rejects `--no-write-lock-file` as a
        // leading global.
        if rest.first().map(String::as_str) == Some("flake") {
            out.push(rest.remove(0));
            if !rest.is_empty() {
                out.push(rest.remove(0));
            }
        } else if !rest.is_empty() {
            out.push(rest.remove(0));
        }

        if requested.no_write_lock_file && self.supports_no_write_lock_file {
            out.push("--no-write-lock-file".to_owned());
        }

        out.extend(rest);
        out
    }
}

/// Locate the `nix` executable via `NXR_NIX` or `PATH`.
///
/// # Errors
///
/// Returns [`NixError::NixNotFound`] when no usable `nix` executable is available.
pub fn locate_nix() -> Result<Utf8PathBuf, NixError> {
    if let Ok(explicit) = std::env::var(NIX_EXECUTABLE_ENV) {
        let path = Utf8PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(NixError::NixNotFound { path });
    }

    let path = which::which("nix").map_err(|_| NixError::NixNotFound {
        path: Utf8PathBuf::from("nix"),
    })?;

    Utf8PathBuf::from_path_buf(path).map_err(|_| NixError::NixNotFound {
        path: Utf8PathBuf::from("nix"),
    })
}

/// Detect the current Nix system string (for example `aarch64-darwin`).
///
/// # Errors
///
/// Returns [`NixError`] when `nix eval` fails or returns an empty system string.
pub fn detect_system(nix: &Utf8Path) -> Result<String, NixError> {
    let args = command::current_system_args();
    let output = run_nix(nix, &args, NixFailureKind::Capability)?;

    let system = String::from_utf8(output).map_err(|_| NixError::InvalidSystemOutput)?;
    let system = system.trim();
    if system.is_empty() {
        return Err(NixError::InvalidSystemOutput);
    }

    Ok(system.to_owned())
}

/// Probe the installed Nix once and negotiate capabilities.
///
/// # Errors
///
/// Returns [`NixError`] when version detection fails.
pub fn detect_capabilities(nix: &Utf8Path) -> Result<NixCapabilities, NixError> {
    let version_output = run_nix_capture(nix, &["--version".to_owned()])?;
    let version =
        parse_nix_version_output(&version_output).ok_or(NixError::InvalidVersionOutput)?;

    let config_json = probe_config_json(nix);
    let help_text = probe_help_text(nix);

    Ok(negotiate_capabilities(
        version,
        &version_output,
        config_json.as_deref(),
        help_text.as_deref(),
    ))
}

/// Build [`NixCapabilities`] from already-captured Nix outputs (unit-test seam).
#[must_use]
pub fn negotiate_capabilities(
    version: NixVersion,
    version_output: &str,
    config_json: Option<&str>,
    help_text: Option<&str>,
) -> NixCapabilities {
    let experimental = experimental_features_from_config(config_json);
    let flakes_enabled = detect_flakes_enabled(version_output, experimental.as_deref(), help_text);

    let at_feature_floor = version >= FEATURE_FLOOR;
    let help = help_text.unwrap_or("");

    let supports_offline = help_mentions(help, "--offline") || at_feature_floor;
    let supports_json_log_format = help_mentions_log_format_json(help) || at_feature_floor;
    let supports_no_write_lock_file =
        flakes_enabled && (help_mentions(help, "--no-write-lock-file") || at_feature_floor);
    let supports_accept_flake_config = flakes_enabled
        && (config_has_setting(config_json, "accept-flake-config")
            || help_mentions(help, "--accept-flake-config")
            || at_feature_floor);

    NixCapabilities {
        version,
        flakes_enabled,
        supports_json_log_format,
        supports_no_write_lock_file,
        supports_offline,
        supports_accept_flake_config,
    }
}

fn detect_flakes_enabled(
    version_output: &str,
    experimental: Option<&[String]>,
    help_text: Option<&str>,
) -> bool {
    if experimental.is_some_and(|features| features.iter().any(|feature| feature == "flakes")) {
        return true;
    }
    // Determinate Nix treats flakes as stable; they need not appear in
    // experimental-features (CI `latest` matrix).
    if version_output.contains("Determinate") {
        return true;
    }
    // Positive probe: `nix flake --help` succeeded and was captured.
    help_text.is_some_and(help_indicates_flake_command)
}

fn help_indicates_flake_command(help: &str) -> bool {
    help.contains("nix flake")
        || help.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed == "flake" || trimmed.starts_with("flake ")
        })
}

/// Parse `nix --version` stdout into a [`NixVersion`].
#[must_use]
pub fn parse_nix_version_output(output: &str) -> Option<NixVersion> {
    let line = output.lines().next()?.trim();
    // Examples: "nix (Nix) 2.34.7", "nix (Lix, like Nix) 2.91.0"
    let version_token = line.split_whitespace().next_back()?;
    parse_nix_version_token(version_token)
}

fn parse_nix_version_token(token: &str) -> Option<NixVersion> {
    let mut parts = token.trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()
        .and_then(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .unwrap_or(0);
    Some(NixVersion {
        major,
        minor,
        patch,
    })
}

/// Run `nix` with `args` and return stdout on success.
///
/// # Errors
///
/// Returns [`NixError`] when `nix` cannot be spawned or exits unsuccessfully.
pub fn run_nix(
    nix: &Utf8Path,
    args: &[String],
    failure_kind: NixFailureKind,
) -> Result<Vec<u8>, NixError> {
    let output = Command::new(nix.as_std_path())
        .args(args)
        .output()
        .map_err(|source| NixError::SpawnFailed {
            nix: nix.to_path_buf(),
            source,
        })?;

    if output.status.success() {
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(NixError::CommandFailed {
        nix: nix.to_path_buf(),
        args: args.to_vec(),
        status: output.status.code(),
        stderr,
        kind: failure_kind,
    })
}

fn run_nix_capture(nix: &Utf8Path, args: &[String]) -> Result<String, NixError> {
    let stdout = run_nix(nix, args, NixFailureKind::Capability)?;
    String::from_utf8(stdout).map_err(|_| NixError::InvalidVersionOutput)
}

fn probe_config_json(nix: &Utf8Path) -> Option<String> {
    for args in [
        vec!["config".to_owned(), "show".to_owned(), "--json".to_owned()],
        vec!["show-config".to_owned(), "--json".to_owned()],
    ] {
        if let Ok(stdout) = run_nix(nix, &args, NixFailureKind::Capability)
            && let Ok(text) = String::from_utf8(stdout)
        {
            return Some(text);
        }
    }
    None
}

fn probe_help_text(nix: &Utf8Path) -> Option<String> {
    let mut combined = String::new();
    for args in [
        vec!["--help".to_owned()],
        vec!["flake".to_owned(), "--help".to_owned()],
        vec!["eval".to_owned(), "--help".to_owned()],
    ] {
        if let Ok(stdout) = run_nix(nix, &args, NixFailureKind::Capability)
            && let Ok(text) = String::from_utf8(stdout)
        {
            combined.push_str(&text);
            combined.push('\n');
        }
    }
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn experimental_features_from_config(config_json: Option<&str>) -> Option<Vec<String>> {
    let raw = config_json?;
    let value: JsonValue = serde_json::from_str(raw).ok()?;
    let setting = value.get("experimental-features")?;
    let features_value = setting.get("value").unwrap_or(setting);
    match features_value {
        JsonValue::Array(items) => Some(
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        JsonValue::String(text) => Some(text.split_whitespace().map(str::to_owned).collect()),
        _ => None,
    }
}

fn config_has_setting(config_json: Option<&str>, key: &str) -> bool {
    let Some(raw) = config_json else {
        return false;
    };
    serde_json::from_str::<JsonValue>(raw)
        .ok()
        .is_some_and(|value| value.get(key).is_some())
}

fn help_mentions(help: &str, flag: &str) -> bool {
    help.split_whitespace().any(|token| token == flag)
        || help.contains(&format!(" {flag} "))
        || help.contains(&format!("\n{flag} "))
        || help.contains(&format!("· {flag}"))
}

fn help_mentions_log_format_json(help: &str) -> bool {
    help_mentions(help, "--log-format") && (help.contains("json") || help.contains("JSON"))
}

#[cfg(test)]
mod tests {
    use super::{
        FEATURE_FLOOR, NixCapabilities, NixVersion, OptionalNixFlags, TESTED_NIX_SUPPORT_FLOOR,
        detect_system, locate_nix, negotiate_capabilities, parse_nix_version_output,
    };

    #[test]
    fn locate_nix_finds_executable_on_path() {
        let nix = locate_nix().expect("nix should be on PATH in dev environments");
        assert!(nix.is_file());
    }

    #[test]
    #[ignore = "requires nix on PATH"]
    fn detect_system_returns_current_platform() {
        let nix = locate_nix().expect("nix");
        let system = detect_system(&nix).expect("detect system");
        assert!(!system.is_empty());
        assert!(system.contains('-'));
    }

    #[test]
    fn parse_nix_version_from_common_outputs() {
        assert_eq!(
            parse_nix_version_output("nix (Nix) 2.34.7\n"),
            Some(NixVersion::new(2, 34, 7))
        );
        assert_eq!(
            parse_nix_version_output("nix (Nix) 2.18.1"),
            Some(NixVersion::new(2, 18, 1))
        );
        assert_eq!(
            parse_nix_version_output("nix (Lix, like Nix) 2.91.0"),
            Some(NixVersion::new(2, 91, 0))
        );
        assert_eq!(
            parse_nix_version_output("nix (Nix) 2.4"),
            Some(NixVersion::new(2, 4, 0))
        );
        assert_eq!(
            parse_nix_version_output("nix (Determinate Nix 3.21.7) 2.34.8\n"),
            Some(NixVersion::new(2, 34, 8))
        );
        assert_eq!(parse_nix_version_output("not-a-version"), None);
    }

    #[test]
    fn negotiate_enables_flakes_from_config_json() {
        let config = r#"{
            "experimental-features": {
                "value": ["nix-command", "flakes"]
            },
            "accept-flake-config": { "value": false }
        }"#;
        let caps = negotiate_capabilities(
            NixVersion::new(2, 18, 1),
            "nix (Nix) 2.18.1\n",
            Some(config),
            Some(
                "--offline\n--log-format format\n--no-write-lock-file\n--accept-flake-config\njson",
            ),
        );
        assert!(caps.flakes_enabled);
        assert!(caps.supports_offline);
        assert!(caps.supports_json_log_format);
        assert!(caps.supports_no_write_lock_file);
        assert!(caps.supports_accept_flake_config);
        assert_eq!(caps.version, NixVersion::new(2, 18, 1));
    }

    #[test]
    fn negotiate_reports_flakes_disabled_clearly() {
        let config = r#"{
            "experimental-features": { "value": ["nix-command"] }
        }"#;
        let caps = negotiate_capabilities(
            NixVersion::new(2, 18, 1),
            "nix (Nix) 2.18.1\n",
            Some(config),
            None,
        );
        assert!(!caps.flakes_enabled);
        assert!(!caps.supports_no_write_lock_file);
        assert!(!caps.supports_accept_flake_config);
        assert!(caps.supports_offline);
        assert!(caps.supports_json_log_format);
    }

    #[test]
    fn negotiate_enables_flakes_for_determinate_without_experimental_flag() {
        let config = r#"{
            "experimental-features": { "value": [] }
        }"#;
        let caps = negotiate_capabilities(
            NixVersion::new(2, 34, 8),
            "nix (Determinate Nix 3.21.7) 2.34.8\n",
            Some(config),
            None,
        );
        assert!(caps.flakes_enabled);
        assert!(caps.supports_no_write_lock_file);
        assert!(caps.supports_accept_flake_config);
    }

    #[test]
    fn negotiate_enables_flakes_from_flake_help_probe() {
        let caps = negotiate_capabilities(
            NixVersion::new(2, 18, 1),
            "nix (Nix) 2.18.1\n",
            None,
            Some("Usage: nix flake <subcommand>\n"),
        );
        assert!(caps.flakes_enabled);
    }

    #[test]
    fn negotiate_falls_back_to_version_floor_without_probes() {
        let caps = negotiate_capabilities(FEATURE_FLOOR, "nix (Nix) 2.4.0\n", None, None);
        assert!(!caps.flakes_enabled);
        assert!(caps.supports_offline);
        assert!(caps.supports_json_log_format);
        assert!(!caps.supports_no_write_lock_file);
    }

    #[test]
    fn select_compatible_globals_drops_unsupported_flags() {
        let caps = NixCapabilities {
            version: NixVersion::new(2, 3, 0),
            flakes_enabled: false,
            supports_json_log_format: false,
            supports_no_write_lock_file: false,
            supports_offline: true,
            supports_accept_flake_config: false,
        };
        let flags = caps.select_compatible_globals(&OptionalNixFlags {
            offline: true,
            no_write_lock_file: true,
            accept_flake_config: true,
            json_log_format: true,
            nix_options: Vec::new(),
            extra_argv: Vec::new(),
        });
        assert_eq!(flags, vec!["--offline".to_owned()]);
    }

    #[test]
    fn select_compatible_globals_includes_nix_options_and_extra_argv() {
        let caps = NixCapabilities::all_supported_for_tests(TESTED_NIX_SUPPORT_FLOOR);
        let flags = caps.select_compatible_globals(&OptionalNixFlags {
            offline: false,
            no_write_lock_file: false,
            accept_flake_config: false,
            json_log_format: false,
            nix_options: vec![("warn-dirty".to_owned(), "false".to_owned())],
            extra_argv: vec!["--refresh".to_owned()],
        });
        assert_eq!(
            flags,
            vec![
                "--option".to_owned(),
                "warn-dirty".to_owned(),
                "false".to_owned(),
                "--refresh".to_owned(),
            ]
        );
    }

    #[test]
    fn apply_optional_flags_places_lock_option_after_verb() {
        let caps = NixCapabilities::all_supported_for_tests(TESTED_NIX_SUPPORT_FLOOR);
        let args = caps.apply_optional_flags(
            vec![
                "flake".to_owned(),
                "show".to_owned(),
                "--json".to_owned(),
                ".".to_owned(),
            ],
            &OptionalNixFlags {
                offline: true,
                no_write_lock_file: true,
                accept_flake_config: false,
                json_log_format: false,
                nix_options: Vec::new(),
                extra_argv: Vec::new(),
            },
        );
        assert_eq!(
            args,
            vec![
                "--offline".to_owned(),
                "flake".to_owned(),
                "show".to_owned(),
                "--no-write-lock-file".to_owned(),
                "--json".to_owned(),
                ".".to_owned(),
            ]
        );
    }

    #[test]
    fn apply_optional_flags_for_run_inserts_after_verb() {
        let caps = NixCapabilities::all_supported_for_tests(TESTED_NIX_SUPPORT_FLOOR);
        let args = caps.apply_optional_flags(
            vec!["run".to_owned(), ".#hello".to_owned()],
            &OptionalNixFlags {
                offline: true,
                no_write_lock_file: false,
                accept_flake_config: false,
                json_log_format: false,
                nix_options: Vec::new(),
                extra_argv: Vec::new(),
            },
        );
        assert_eq!(
            args,
            vec![
                "--offline".to_owned(),
                "run".to_owned(),
                ".#hello".to_owned()
            ]
        );
    }

    #[test]
    fn tested_support_floor_is_documented_constant() {
        assert_eq!(TESTED_NIX_SUPPORT_FLOOR, NixVersion::new(2, 18, 0));
    }
}
