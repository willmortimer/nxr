//! Flake output discovery via `nix flake show --json`.

use std::collections::BTreeMap;

use camino::Utf8Path;
use nxr_core::{App, FlakeOutput};
use serde_json::Value as JsonValue;

use crate::capabilities::{NixFailureKind, run_nix};
use crate::command;
use crate::{NixError, ParseAppsError};

/// Which flake output table to parse from `nix flake show --json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTable {
    /// `apps.<system>.*` (`type == "app"`).
    Apps,
    /// `packages.<system>.*` (`type == "derivation"`).
    Packages,
    /// `checks.<system>.*` (`type == "derivation"`).
    Checks,
    /// `devShells.<system>.*` (`type == "derivation"`).
    DevShells,
}

impl OutputTable {
    #[must_use]
    pub const fn show_key(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::Packages => "packages",
            Self::Checks => "checks",
            Self::DevShells => "devShells",
        }
    }

    #[must_use]
    pub const fn attr_prefix(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::Packages => "packages",
            Self::Checks => "checks",
            Self::DevShells => "devShells",
        }
    }

    #[must_use]
    pub const fn expected_type(self) -> &'static str {
        match self {
            Self::Apps => "app",
            Self::Packages | Self::Checks | Self::DevShells => "derivation",
        }
    }
}

/// Discover apps for `system` from `flake_ref`.
///
/// Uses a single `nix flake show --json` evaluation. Descriptions are taken from
/// the show output when present (`meta.description` is surfaced by Nix).
///
/// # Errors
///
/// Returns [`NixError`] when `nix flake show` fails or its JSON cannot be parsed.
pub fn discover_apps(nix: &Utf8Path, system: &str, flake_ref: &str) -> Result<Vec<App>, NixError> {
    let args = command::flake_show_args(flake_ref);
    discover_apps_with_args(nix, system, flake_ref, &args)
}

/// Discover apps using a pre-built (capability-aware) argv.
///
/// # Errors
///
/// Returns [`NixError`] when `nix` fails or its JSON cannot be parsed.
pub fn discover_apps_with_args(
    nix: &Utf8Path,
    system: &str,
    flake_ref: &str,
    args: &[String],
) -> Result<Vec<App>, NixError> {
    let stdout = run_nix(nix, args, NixFailureKind::Evaluation)?;
    let show: JsonValue =
        serde_json::from_slice(&stdout).map_err(|source| NixError::InvalidJson { source })?;
    parse_apps_from_flake_show(&show, flake_ref, system).map_err(NixError::ParseApps)
}

/// Discover non-app flake outputs using a pre-built argv.
///
/// # Errors
///
/// Returns [`NixError`] when `nix` fails or its JSON cannot be parsed.
pub fn discover_outputs_with_args(
    nix: &Utf8Path,
    system: &str,
    flake_ref: &str,
    table: OutputTable,
    args: &[String],
) -> Result<Vec<FlakeOutput>, NixError> {
    let stdout = run_nix(nix, args, NixFailureKind::Evaluation)?;
    let show: JsonValue =
        serde_json::from_slice(&stdout).map_err(|source| NixError::InvalidJson { source })?;
    parse_outputs_from_flake_show(&show, flake_ref, system, table).map_err(NixError::ParseApps)
}

/// Parse `apps.<system>.*` entries from `nix flake show --json` output.
///
/// # Errors
///
/// Returns [`ParseAppsError`] when the show JSON has an unexpected structure.
pub fn parse_apps_from_flake_show(
    show: &JsonValue,
    flake_ref: &str,
    system: &str,
) -> Result<Vec<App>, ParseAppsError> {
    let outputs = parse_outputs_from_flake_show(show, flake_ref, system, OutputTable::Apps)?;
    Ok(outputs
        .into_iter()
        .map(|output| App {
            name: output.name,
            attr_path: output.attr_path,
            flake_ref: output.flake_ref,
            system: output.system,
            description: output.description,
            is_default: output.is_default,
            metadata: BTreeMap::new(),
        })
        .collect())
}

/// Parse a flake output table from `nix flake show --json`.
///
/// # Errors
///
/// Returns [`ParseAppsError`] when the show JSON has an unexpected structure.
pub fn parse_outputs_from_flake_show(
    show: &JsonValue,
    flake_ref: &str,
    system: &str,
    table: OutputTable,
) -> Result<Vec<FlakeOutput>, ParseAppsError> {
    let Some(entries) = show
        .get(table.show_key())
        .and_then(|root| root.get(system))
        .and_then(JsonValue::as_object)
    else {
        return Ok(Vec::new());
    };

    let expected_type = table.expected_type();
    let mut outputs = Vec::new();
    for (name, entry) in entries {
        let Some(entry_type) = entry.get("type").and_then(JsonValue::as_str) else {
            continue;
        };
        if entry_type != expected_type {
            continue;
        }

        let description = entry
            .get("description")
            .and_then(JsonValue::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);

        outputs.push(FlakeOutput {
            name: name.clone(),
            attr_path: format!("{}.{system}.{name}", table.attr_prefix()),
            flake_ref: flake_ref.to_owned(),
            system: system.to_owned(),
            description,
            is_default: name == "default",
        });
    }

    outputs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{OutputTable, parse_apps_from_flake_show, parse_outputs_from_flake_show};
    use nxr_core::App;

    const BASIC_APPS_SHOW: &str =
        include_str!("../../../tests/fixtures/basic-apps-flake-show.json");

    #[test]
    fn parse_basic_apps_fixture_is_sorted_and_marks_default() {
        let show: serde_json::Value =
            serde_json::from_str(BASIC_APPS_SHOW).expect("parse fixture JSON");
        let apps = parse_apps_from_flake_show(&show, ".", "aarch64-darwin").expect("parse apps");

        let names: Vec<&str> = apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["default", "echo-args", "fail", "hello", "pwd", "succeed"]
        );

        let default = apps
            .iter()
            .find(|app| app.name == "default")
            .expect("default app");
        assert!(default.is_default);
        assert_eq!(default.description.as_deref(), Some("Print a greeting"));

        let hello = apps.iter().find(|app| app.name == "hello").expect("hello");
        assert!(!hello.is_default);
        assert_eq!(hello.attr_path, "apps.aarch64-darwin.hello");
    }

    #[test]
    fn parse_skips_non_app_entries() {
        let show = json!({
            "apps": {
                "aarch64-darwin": {
                    "valid": { "type": "app", "description": "ok" },
                    "packages": { "type": "derivation" },
                    "missing-type": { "description": "skip me" }
                }
            }
        });

        let apps = parse_apps_from_flake_show(&show, ".", "aarch64-darwin").expect("parse apps");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "valid");
    }

    #[test]
    fn parse_missing_system_returns_empty_list() {
        let show = json!({ "apps": { "x86_64-linux": {} } });
        let apps = parse_apps_from_flake_show(&show, ".", "aarch64-darwin").expect("parse apps");
        assert!(apps.is_empty());
    }

    #[test]
    fn parse_app_metadata_fixture_descriptions() {
        let show = json!({
            "apps": {
                "aarch64-darwin": {
                    "lint": { "type": "app", "description": "Run static analysis" },
                    "test": { "type": "app", "description": "Run the test suite" },
                    "deploy": { "type": "app", "description": "Deploy the current revision" }
                }
            }
        });

        let apps = parse_apps_from_flake_show(&show, "./fixtures/app-metadata", "aarch64-darwin")
            .expect("parse apps");

        assert_eq!(
            apps,
            vec![
                App {
                    name: "deploy".to_owned(),
                    attr_path: "apps.aarch64-darwin.deploy".to_owned(),
                    flake_ref: "./fixtures/app-metadata".to_owned(),
                    system: "aarch64-darwin".to_owned(),
                    description: Some("Deploy the current revision".to_owned()),
                    is_default: false,
                    metadata: BTreeMap::new(),
                },
                App {
                    name: "lint".to_owned(),
                    attr_path: "apps.aarch64-darwin.lint".to_owned(),
                    flake_ref: "./fixtures/app-metadata".to_owned(),
                    system: "aarch64-darwin".to_owned(),
                    description: Some("Run static analysis".to_owned()),
                    is_default: false,
                    metadata: BTreeMap::new(),
                },
                App {
                    name: "test".to_owned(),
                    attr_path: "apps.aarch64-darwin.test".to_owned(),
                    flake_ref: "./fixtures/app-metadata".to_owned(),
                    system: "aarch64-darwin".to_owned(),
                    description: Some("Run the test suite".to_owned()),
                    is_default: false,
                    metadata: BTreeMap::new(),
                },
            ]
        );
    }

    #[test]
    fn parse_packages_checks_and_shells() {
        let show = json!({
            "packages": {
                "aarch64-darwin": {
                    "default": { "type": "derivation", "description": "Default package" },
                    "tool": { "type": "derivation", "description": "A tool" },
                    "skip": { "type": "app" }
                }
            },
            "checks": {
                "aarch64-darwin": {
                    "fmt": { "type": "derivation", "description": "" },
                    "empty": {}
                }
            },
            "devShells": {
                "aarch64-darwin": {
                    "default": { "type": "derivation" },
                    "backend": { "type": "derivation", "description": "Backend shell" }
                }
            }
        });

        let packages =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::Packages)
                .expect("packages");
        assert_eq!(
            packages.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
            vec!["default", "tool"]
        );
        assert!(packages[0].is_default);
        assert_eq!(packages[0].attr_path, "packages.aarch64-darwin.default");

        let checks =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::Checks)
                .expect("checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "fmt");
        assert_eq!(checks[0].description, None);

        let shells =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::DevShells)
                .expect("shells");
        assert_eq!(
            shells.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
            vec!["backend", "default"]
        );
        assert_eq!(shells[0].attr_path, "devShells.aarch64-darwin.backend");
    }
}
