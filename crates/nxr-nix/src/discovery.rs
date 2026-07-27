//! Flake output discovery via `nix flake show --json`.

use std::collections::BTreeMap;

use camino::Utf8Path;
use nxr_core::{App, FlakeOutput};
use serde_json::Value as JsonValue;

use crate::capabilities::{NixFailureKind, run_nix};
use crate::command;
pub use crate::inventory::OutputTable;
use crate::inventory::{list_standard_outputs, parse_flake_inventory};
use crate::{NixError, ParseAppsError};

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
/// Supports both upstream Nix's legacy shape (`apps.<system>.<name>.type`) and
/// Determinate Nix inventory v2 (`inventory.apps.output.children…what`).
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
    let inventory = parse_flake_inventory(show);
    let entries = list_standard_outputs(&inventory, table, system);
    Ok(entries
        .into_iter()
        .map(|entry| FlakeOutput {
            name: entry.name.clone(),
            attr_path: format!("{}.{system}.{}", table.attr_prefix(), entry.name),
            flake_ref: flake_ref.to_owned(),
            system: system.to_owned(),
            description: entry.description,
            is_default: entry.is_default,
        })
        .collect())
}

/// Whether `nix flake show --json` exposes an `nxr` output (task documents may exist).
///
/// Used to skip task `eval` when the flake has no `nxr` output (apps-only flakes).
/// Inventory v2 may list `nxr` without `output.children` (`unknown: true`); consult
/// the raw `inventory.nxr` key as well as the parsed inventory tree.
#[must_use]
pub fn flake_show_has_nxr_for_system(show: &JsonValue, system: &str) -> bool {
    let inventory = parse_flake_inventory(show);
    if inventory.outputs.contains_key("nxr") {
        return true;
    }
    if show
        .get("inventory")
        .and_then(|inv| inv.get("nxr"))
        .is_some()
    {
        return true;
    }
    show.get("nxr").and_then(|nxr| nxr.get(system)).is_some()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{OutputTable, parse_apps_from_flake_show, parse_outputs_from_flake_show};
    use crate::inventory::parse_flake_inventory;
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
    fn flake_show_has_nxr_for_system_detects_legacy_and_inventory_shapes() {
        let basic: serde_json::Value =
            serde_json::from_str(BASIC_APPS_SHOW).expect("parse basic-apps fixture");
        assert!(!super::flake_show_has_nxr_for_system(
            &basic,
            "aarch64-darwin"
        ));

        let with_nxr = json!({
            "nxr": {
                "aarch64-darwin": { "schema_version": 1, "tasks": {} }
            }
        });
        assert!(super::flake_show_has_nxr_for_system(
            &with_nxr,
            "aarch64-darwin"
        ));
        assert!(super::flake_show_has_nxr_for_system(
            &with_nxr,
            "x86_64-linux"
        ));

        let inventory_unknown_nxr = json!({
            "version": 2,
            "inventory": {
                "nxr": { "unknown": true }
            }
        });
        assert!(super::flake_show_has_nxr_for_system(
            &inventory_unknown_nxr,
            "aarch64-darwin"
        ));
    }

    #[test]
    fn parse_determinate_inventory_v2_apps() {
        let show = json!({
            "version": 2,
            "inventory": {
                "apps": {
                    "doc": "The apps output",
                    "output": {
                        "children": {
                            "x86_64-linux": {
                                "forSystems": ["x86_64-linux"],
                                "children": {
                                    "shared-check": {
                                        "what": "app",
                                        "shortDescription": "Validate shared library inputs",
                                        "forSystems": ["x86_64-linux"]
                                    },
                                    "filtered-out": {
                                        "what": "package"
                                    },
                                    "default": {
                                        "what": "app",
                                        "shortDescription": ""
                                    }
                                }
                            },
                            "aarch64-darwin": {
                                "filtered": true
                            }
                        }
                    }
                }
            }
        });

        let apps = parse_apps_from_flake_show(&show, "./fixtures/affected-deps", "x86_64-linux")
            .expect("parse apps");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "default");
        assert!(apps[0].is_default);
        assert_eq!(apps[0].description, None);
        assert_eq!(apps[1].name, "shared-check");
        assert_eq!(
            apps[1].description.as_deref(),
            Some("Validate shared library inputs")
        );
        assert_eq!(apps[1].attr_path, "apps.x86_64-linux.shared-check");

        let other =
            parse_apps_from_flake_show(&show, ".", "aarch64-darwin").expect("filtered system");
        assert!(other.is_empty());
    }

    #[test]
    fn parse_determinate_inventory_v2_packages_checks_shells() {
        let show = json!({
            "version": 2,
            "inventory": {
                "packages": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "tool": {
                                        "what": "package",
                                        "shortDescription": "A tool"
                                    }
                                }
                            }
                        }
                    }
                },
                "checks": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "fmt": { "what": "CI test", "shortDescription": "" }
                                }
                            }
                        }
                    }
                },
                "devShells": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "backend": {
                                        "what": "development environment",
                                        "shortDescription": "Backend shell"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let packages =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::Packages)
                .expect("packages");
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "tool");
        assert_eq!(packages[0].description.as_deref(), Some("A tool"));

        let checks =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::Checks)
                .expect("checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].description, None);

        let shells =
            parse_outputs_from_flake_show(&show, ".", "aarch64-darwin", OutputTable::DevShells)
                .expect("shells");
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].description.as_deref(), Some("Backend shell"));
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

    #[test]
    fn parse_unknown_outputs_preserved_in_ast_without_affecting_standard_lists() {
        let show = json!({
            "apps": {
                "aarch64-darwin": {
                    "hello": { "type": "app", "description": "Hi" }
                }
            },
            "customWorkflow": {
                "aarch64-darwin": {
                    "plan": { "type": "unknown", "description": "CI plan" }
                }
            },
            "version": 2,
            "inventory": {
                "nxr": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "test": {
                                        "what": "NXR task",
                                        "shortDescription": "Run tests"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let inventory = parse_flake_inventory(&show);
        assert!(inventory.outputs.contains_key("customWorkflow"));
        assert!(inventory.outputs.contains_key("nxr"));

        let apps = parse_apps_from_flake_show(&show, ".", "aarch64-darwin").expect("apps");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "hello");
    }
}
