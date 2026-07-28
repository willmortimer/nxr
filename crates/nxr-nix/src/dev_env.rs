//! Parse `nix print-dev-env --json` into process-compatible environment fields.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Protocol version for normalized dev-environment snapshots (ADR-0171).
pub const DEV_ENV_PROTOCOL_VERSION: u32 = 1;

/// Shell semantics that cannot be represented as a process environment.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedDevEnvFeature {
    /// Top-level `bashFunctions` entries (shell functions).
    BashFunctions,
    /// Variables with type `var` (non-exported shell internals).
    ShellVariable,
    /// Variables with type `array` (hook lists and other arrays).
    ArrayVariable,
    /// Non-empty exported `shellHook` (must run inside `nix develop`).
    ShellHook,
    /// Variable type not classified as process-representable.
    UnknownVariableType { name: String, variable_type: String },
}

impl UnsupportedDevEnvFeature {
    /// Whether this feature forces `nix develop -c` fallback.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        match self {
            Self::BashFunctions | Self::ShellVariable | Self::ArrayVariable => false,
            Self::ShellHook | Self::UnknownVariableType { .. } => true,
        }
    }
}

/// Normalized process-compatible development environment from `print-dev-env --json`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DevEnvironment {
    /// Exported process environment variables (excluding `PATH`).
    pub variables: BTreeMap<String, String>,
    /// `PATH` entries in order (from the exported `PATH` variable).
    pub path_entries: Vec<String>,
    /// Features requiring `nix develop -c` fallback when non-empty.
    pub unsupported_features: Vec<UnsupportedDevEnvFeature>,
}

impl DevEnvironment {
    /// Whether the snapshot can be used for direct spawn without develop-wrap.
    ///
    /// Skipped features (functions, shell internals, hook arrays) are recorded in
    /// [`Self::unsupported_features`] for explain/telemetry but do not block spawn.
    #[must_use]
    pub fn is_process_compatible(&self) -> bool {
        !self
            .unsupported_features
            .iter()
            .any(UnsupportedDevEnvFeature::is_blocking)
    }
}

/// Errors while parsing `nix print-dev-env --json` output.
#[derive(Debug, thiserror::Error)]
pub enum DevEnvParseError {
    /// Root JSON was not an object.
    #[error("print-dev-env JSON root must be an object")]
    InvalidRoot,

    /// `variables` was present but not an object.
    #[error("print-dev-env `variables` must be an object")]
    InvalidVariables,

    /// A variable entry was not an object with `type` and `value`.
    #[error("print-dev-env variable `{name}` has invalid structure")]
    InvalidVariableEntry { name: String },

    /// JSON could not be deserialized.
    #[error("print-dev-env output was not valid JSON: {source}")]
    InvalidJson {
        #[from]
        source: serde_json::Error,
    },
}

/// Parse `nix print-dev-env --json` stdout into a [`DevEnvironment`].
///
/// Exported string variables become `variables`; `PATH` is split into
/// `path_entries`. Functions, hook arrays, and non-exported shell variables are
/// recorded in `unsupported_features` for explain/telemetry. Only blocking
/// features (non-empty `shellHook`, unknown variable types) force develop-wrap.
pub fn parse_print_dev_env_json(json: &str) -> Result<DevEnvironment, DevEnvParseError> {
    let root: JsonValue = serde_json::from_str(json)?;
    let root_obj = root.as_object().ok_or(DevEnvParseError::InvalidRoot)?;

    let mut env = DevEnvironment::default();

    if let Some(functions) = root_obj.get("bashFunctions") {
        if functions.as_object().is_some_and(|map| !map.is_empty()) {
            env.unsupported_features
                .push(UnsupportedDevEnvFeature::BashFunctions);
        }
    }

    let variables = root_obj
        .get("variables")
        .map(|value| value.as_object().ok_or(DevEnvParseError::InvalidVariables))
        .transpose()?;

    if let Some(variable_map) = variables {
        for (name, entry) in variable_map {
            classify_variable(name.clone(), entry, &mut env)?;
        }
    }

    env.unsupported_features.sort();
    env.unsupported_features.dedup();
    Ok(env)
}

/// Stable snake_case label for a skipped/unsupported feature (telemetry and cache).
#[must_use]
pub fn unsupported_feature_label(feature: &UnsupportedDevEnvFeature) -> &'static str {
    match feature {
        UnsupportedDevEnvFeature::BashFunctions => "bash_functions",
        UnsupportedDevEnvFeature::ShellVariable => "shell_variable",
        UnsupportedDevEnvFeature::ArrayVariable => "array_variable",
        UnsupportedDevEnvFeature::ShellHook => "shell_hook",
        UnsupportedDevEnvFeature::UnknownVariableType { .. } => "unknown_variable_type",
    }
}

fn classify_variable(
    name: String,
    entry: &JsonValue,
    env: &mut DevEnvironment,
) -> Result<(), DevEnvParseError> {
    let entry_obj = entry
        .as_object()
        .ok_or(DevEnvParseError::InvalidVariableEntry { name: name.clone() })?;
    let variable_type = entry_obj
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let value = entry_obj.get("value");

    match variable_type {
        "exported" => {
            let text = value
                .and_then(JsonValue::as_str)
                .ok_or_else(|| DevEnvParseError::InvalidVariableEntry { name: name.clone() })?;
            if name == "PATH" {
                env.path_entries = split_path_entries(text);
            } else {
                if name == "shellHook" && !text.is_empty() {
                    env.unsupported_features
                        .push(UnsupportedDevEnvFeature::ShellHook);
                }
                env.variables.insert(name, text.to_owned());
            }
        }
        "var" => env
            .unsupported_features
            .push(UnsupportedDevEnvFeature::ShellVariable),
        "array" => env
            .unsupported_features
            .push(UnsupportedDevEnvFeature::ArrayVariable),
        other => {
            env.unsupported_features
                .push(UnsupportedDevEnvFeature::UnknownVariableType {
                    name,
                    variable_type: other.to_owned(),
                });
        }
    }

    Ok(())
}

fn split_path_entries(path: &str) -> Vec<String> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split(':').map(str::to_owned).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{DevEnvironment, UnsupportedDevEnvFeature, parse_print_dev_env_json};

    const MINIMAL_EXPORTED: &str = r#"{
        "bashFunctions": {},
        "variables": {
            "NXR_FIXTURE_SHELL_MARKER": {
                "type": "exported",
                "value": "inside-default-shell"
            },
            "AR": {
                "type": "exported",
                "value": "ar"
            }
        }
    }"#;

    const WITH_PATH: &str = r#"{
        "bashFunctions": {},
        "variables": {
            "PATH": {
                "type": "exported",
                "value": "/nix/store/abc/bin:/nix/store/def/bin"
            },
            "NXR_FIXTURE_SHELL_MARKER": {
                "type": "exported",
                "value": "marker"
            }
        }
    }"#;

    const WITH_BASH_FUNCTIONS: &str = r#"{
        "bashFunctions": {
            "buildPhase": "runHook preBuild"
        },
        "variables": {
            "AR": {
                "type": "exported",
                "value": "ar"
            }
        }
    }"#;

    const WITH_ARRAY_HOOKS: &str = r#"{
        "bashFunctions": {},
        "variables": {
            "envBuildHostHooks": {
                "type": "array",
                "value": ["_updateSourceDateEpochFromSourceRoot"]
            },
            "AR": {
                "type": "exported",
                "value": "ar"
            }
        }
    }"#;

    const WITH_SHELL_VARS: &str = r#"{
        "bashFunctions": {},
        "variables": {
            "BASH": {
                "type": "var",
                "value": "/bin/bash"
            },
            "AR": {
                "type": "exported",
                "value": "ar"
            }
        }
    }"#;

    #[test]
    fn parse_exported_variables_only_is_process_compatible() {
        let env = parse_print_dev_env_json(MINIMAL_EXPORTED).expect("parse minimal");
        assert!(env.is_process_compatible());
        assert_eq!(
            env.variables.get("NXR_FIXTURE_SHELL_MARKER"),
            Some(&"inside-default-shell".to_owned())
        );
        assert_eq!(env.variables.get("AR"), Some(&"ar".to_owned()));
        assert!(env.path_entries.is_empty());
        assert!(env.unsupported_features.is_empty());
    }

    #[test]
    fn parse_path_into_entries_excludes_path_from_variables() {
        let env = parse_print_dev_env_json(WITH_PATH).expect("parse path");
        assert!(env.is_process_compatible());
        assert!(!env.variables.contains_key("PATH"));
        assert_eq!(
            env.path_entries,
            vec![
                "/nix/store/abc/bin".to_owned(),
                "/nix/store/def/bin".to_owned(),
            ]
        );
        assert_eq!(
            env.variables.get("NXR_FIXTURE_SHELL_MARKER"),
            Some(&"marker".to_owned())
        );
    }

    #[test]
    fn parse_empty_path_yields_empty_entries() {
        let json = r#"{
            "bashFunctions": {},
            "variables": {
                "PATH": { "type": "exported", "value": "" }
            }
        }"#;
        let env = parse_print_dev_env_json(json).expect("parse empty path");
        assert!(env.path_entries.is_empty());
        assert!(env.is_process_compatible());
    }

    #[test]
    fn bash_functions_are_skipped_not_blocking() {
        let env = parse_print_dev_env_json(WITH_BASH_FUNCTIONS).expect("parse functions");
        assert!(env.is_process_compatible());
        assert!(
            env.unsupported_features
                .contains(&UnsupportedDevEnvFeature::BashFunctions)
        );
        assert_eq!(env.variables.get("AR"), Some(&"ar".to_owned()));
    }

    #[test]
    fn array_variables_are_skipped_not_blocking() {
        let env = parse_print_dev_env_json(WITH_ARRAY_HOOKS).expect("parse arrays");
        assert!(env.is_process_compatible());
        assert!(
            env.unsupported_features
                .contains(&UnsupportedDevEnvFeature::ArrayVariable)
        );
    }

    #[test]
    fn shell_variables_are_skipped_not_blocking() {
        let env = parse_print_dev_env_json(WITH_SHELL_VARS).expect("parse shell vars");
        assert!(env.is_process_compatible());
        assert!(
            env.unsupported_features
                .contains(&UnsupportedDevEnvFeature::ShellVariable)
        );
    }

    #[test]
    fn non_empty_shell_hook_blocks_process_spawn() {
        let json = r#"{
            "bashFunctions": {},
            "variables": {
                "shellHook": { "type": "exported", "value": "export FOO=1" },
                "AR": { "type": "exported", "value": "ar" }
            }
        }"#;
        let env = parse_print_dev_env_json(json).expect("parse shell hook");
        assert!(!env.is_process_compatible());
        assert!(
            env.unsupported_features
                .contains(&UnsupportedDevEnvFeature::ShellHook)
        );
    }

    #[test]
    fn fixture_with_bash_functions_and_exports_is_process_spawnable() {
        let json = r#"{
            "bashFunctions": {
                "buildPhase": "runHook preBuild"
            },
            "variables": {
                "PATH": {
                    "type": "exported",
                    "value": "/nix/store/abc/bin:/nix/store/def/bin"
                },
                "NXR_FIXTURE_SHELL_MARKER": {
                    "type": "exported",
                    "value": "inside-default-shell"
                },
                "BASH": { "type": "var", "value": "/bin/bash" },
                "envBuildHostHooks": {
                    "type": "array",
                    "value": ["_updateSourceDateEpochFromSourceRoot"]
                }
            }
        }"#;
        let env = parse_print_dev_env_json(json).expect("parse realistic shell");
        assert!(env.is_process_compatible());
        assert_eq!(
            env.variables.get("NXR_FIXTURE_SHELL_MARKER"),
            Some(&"inside-default-shell".to_owned())
        );
        assert!(
            env.unsupported_features
                .contains(&UnsupportedDevEnvFeature::BashFunctions)
        );
    }

    #[test]
    fn unknown_variable_type_is_unsupported() {
        let json = r#"{
            "bashFunctions": {},
            "variables": {
                "weird": { "type": "opaque", "value": "x" }
            }
        }"#;
        let env = parse_print_dev_env_json(json).expect("parse unknown type");
        assert!(!env.is_process_compatible());
        assert!(env.unsupported_features.contains(
            &UnsupportedDevEnvFeature::UnknownVariableType {
                name: "weird".to_owned(),
                variable_type: "opaque".to_owned(),
            }
        ));
    }

    #[test]
    fn invalid_json_errors() {
        let error = parse_print_dev_env_json("not json").expect_err("invalid json");
        assert!(matches!(error, super::DevEnvParseError::InvalidJson { .. }));
    }

    #[test]
    fn missing_variables_is_compatible_empty_env() {
        let env = parse_print_dev_env_json(r#"{"bashFunctions": {}}"#).expect("parse no vars");
        assert_eq!(env, DevEnvironment::default());
        assert!(env.is_process_compatible());
    }
}
