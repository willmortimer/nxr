//! Typed task parameter resolution at spawn time.
//!
//! Parameter **values** are injected as `NXR_PARAM_<NAME>` environment variables
//! when a task child is spawned. Plans and events carry parameter names only.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::schema::{SchemaError, TaskParameter, TaskParameterType};

/// Prefix for spawn-time task parameter environment variables.
pub const PARAM_ENV_PREFIX: &str = "NXR_PARAM_";

/// Canonical parameter and matrix attribute name pattern: `[a-z][a-z0-9_]*`.
pub(crate) fn is_canonical_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        }
        _ => false,
    }
}

/// Errors while resolving typed task parameters for spawn.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParameterError {
    /// Parameter metadata or resolved value failed validation.
    #[error("task {task}: parameters.{name}: {message}")]
    Invalid {
        task: String,
        name: String,
        message: String,
    },
    /// A parameter has no default and was not provided via `--set`, env, or prompt.
    #[error(
        "task {task}: required parameter {name} is unset (pass --set {name}=…, set {env_name}, declare a default, or run interactively on a TTY)"
    )]
    Missing {
        task: String,
        name: String,
        env_name: String,
    },
    /// `--set` named a parameter that is not declared on any task in the plan.
    #[error("unknown parameter --set {name} (not declared on any task in this plan)")]
    UnknownSet { name: String },
    /// Interactive prompt failed or was cancelled.
    #[error("task {task}: parameter {name}: {message}")]
    Prompt {
        task: String,
        name: String,
        message: String,
    },
}

/// Build the spawn-time environment variable name for a task parameter.
#[must_use]
pub fn parameter_env_name(name: &str) -> String {
    format!("{}{}", PARAM_ENV_PREFIX, name.to_ascii_uppercase())
}

/// Sorted parameter names for plan metadata (values never included).
#[must_use]
pub fn parameter_names(parameters: &BTreeMap<String, TaskParameter>) -> Vec<String> {
    let mut names: Vec<String> = parameters.keys().cloned().collect();
    names.sort();
    names
}

/// Resolve spawn-time `NXR_PARAM_*` assignments for a task definition.
///
/// Caller environment values take precedence over schema defaults. Values are
/// validated against each parameter's declared type before spawn.
///
/// # Errors
///
/// Returns [`ParameterError`] when a required parameter is unset or a value is invalid.
pub fn resolve_task_parameter_env(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    resolve_task_parameter_env_with(task, parameters, |env_name| {
        std::env::var(env_name)
            .ok()
            .filter(|value| !value.is_empty())
    })
}

/// Resolve parameters with CLI `--set name=value` overrides (param name keys).
///
/// Lookup order: `cli_sets[name]` → caller env `NXR_PARAM_*` → schema default →
/// optional `prompt` → [`ParameterError::Missing`].
///
/// # Errors
///
/// Returns [`ParameterError`] when a required parameter is unset or a value is invalid.
pub fn resolve_task_parameter_env_layered(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
    cli_sets: &BTreeMap<String, String>,
    lookup_env: impl Fn(&str) -> Option<String>,
    mut prompt: Option<&mut dyn FnMut(&str, &TaskParameter) -> Result<String, ParameterError>>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    let mut env = BTreeMap::new();
    for (name, definition) in parameters {
        let env_name = parameter_env_name(name);
        let raw = cli_sets
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| lookup_env(&env_name));
        let value = match raw {
            Some(value) if !value.is_empty() => value,
            _ => match definition.default.as_ref() {
                Some(default) => json_default_to_string(task, name, definition, default)?,
                None => {
                    if let Some(prompt) = prompt.as_mut() {
                        prompt(name, definition)?
                    } else {
                        return Err(ParameterError::Missing {
                            task: task.to_owned(),
                            name: name.to_owned(),
                            env_name,
                        });
                    }
                }
            },
        };
        let normalized = normalize_parameter_value(task, name, definition, &value)?;
        env.insert(env_name, normalized);
    }
    Ok(env)
}

/// Normalized parameter values keyed by schema parameter name.
#[must_use]
pub fn parameter_values_from_env(
    parameters: &BTreeMap<String, TaskParameter>,
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    parameters
        .keys()
        .map(|name| {
            let env_name = parameter_env_name(name);
            let value = env.get(&env_name).cloned().unwrap_or_default();
            (name.clone(), value)
        })
        .collect()
}

/// Resolve normalized parameter values keyed by schema parameter name.
///
/// # Errors
///
/// Returns [`ParameterError`] when a required parameter is unset or a value is invalid.
pub fn resolve_task_parameter_values(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    let env = resolve_task_parameter_env(task, parameters)?;
    Ok(parameter_values_from_env(parameters, &env))
}

/// Like [`resolve_task_parameter_env`] with an explicit caller-env lookup hook.
pub fn resolve_task_parameter_env_with(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    resolve_task_parameter_env_layered(task, parameters, &BTreeMap::new(), lookup, None)
}

fn json_default_to_string(
    task: &str,
    name: &str,
    definition: &TaskParameter,
    default: &JsonValue,
) -> Result<String, ParameterError> {
    match definition.param_type {
        TaskParameterType::String => match default {
            JsonValue::String(value) => Ok(value.clone()),
            _ => Err(parameter_invalid(task, name, "default must be a string")),
        },
        TaskParameterType::Choice => match default {
            JsonValue::String(value) => Ok(value.clone()),
            _ => Err(parameter_invalid(task, name, "default must be a string")),
        },
        TaskParameterType::Boolean => match default {
            JsonValue::Bool(value) => Ok(if *value { "true" } else { "false" }.to_owned()),
            _ => Err(parameter_invalid(task, name, "default must be a boolean")),
        },
    }
}

fn normalize_parameter_value(
    task: &str,
    name: &str,
    definition: &TaskParameter,
    value: &str,
) -> Result<String, ParameterError> {
    match definition.param_type {
        TaskParameterType::String => Ok(value.to_owned()),
        TaskParameterType::Choice => {
            let values = definition
                .values
                .as_ref()
                .filter(|values| !values.is_empty())
                .ok_or_else(|| parameter_invalid(task, name, "choice parameters require values"))?;
            if values.iter().any(|allowed| allowed == value) {
                Ok(value.to_owned())
            } else {
                Err(parameter_invalid(
                    task,
                    name,
                    format!("value must be one of {:?}", values),
                ))
            }
        }
        TaskParameterType::Boolean => {
            normalize_boolean(value).map_err(|message| ParameterError::Invalid {
                task: task.to_owned(),
                name: name.to_owned(),
                message,
            })
        }
    }
}

fn normalize_boolean(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok("true".to_owned()),
        "false" | "0" | "no" | "off" => Ok("false".to_owned()),
        _ => Err(format!(
            "boolean parameter must be true or false (got {value})"
        )),
    }
}

fn parameter_invalid(task: &str, name: &str, message: impl Into<String>) -> ParameterError {
    ParameterError::Invalid {
        task: task.to_owned(),
        name: name.to_owned(),
        message: message.into(),
    }
}

/// Validate typed parameter metadata during schema load (schema v2).
pub fn validate_task_parameters(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
) -> Result<(), SchemaError> {
    let mut seen_normalized = BTreeMap::new();
    for name in parameters.keys() {
        if name.trim().is_empty() {
            return Err(SchemaError::InvalidParameter {
                task: task.to_owned(),
                name: name.clone(),
                message: "name must not be empty".to_owned(),
            });
        }
        let normalized = name.to_ascii_uppercase();
        if let Some(existing) = seen_normalized.get(&normalized) {
            return Err(SchemaError::InvalidParameter {
                task: task.to_owned(),
                name: name.clone(),
                message: format!(
                    "name collides with parameter `{existing}` after normalization to {PARAM_ENV_PREFIX}{normalized}"
                ),
            });
        }
        seen_normalized.insert(normalized, name.clone());
    }

    for (name, definition) in parameters {
        if !is_canonical_ident(name) {
            return Err(SchemaError::InvalidParameter {
                task: task.to_owned(),
                name: name.clone(),
                message: "name must match [a-z][a-z0-9_]*".to_owned(),
            });
        }
        match definition.param_type {
            TaskParameterType::Choice => {
                let values = definition
                    .values
                    .as_ref()
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "choice parameters require values".to_owned(),
                    })?;
                if let Some(default) = &definition.default {
                    let default_str = match default {
                        JsonValue::String(value) => value.as_str(),
                        _ => {
                            return Err(SchemaError::InvalidParameter {
                                task: task.to_owned(),
                                name: name.to_owned(),
                                message: "default must be a string".to_owned(),
                            });
                        }
                    };
                    if !values.iter().any(|allowed| allowed == default_str) {
                        return Err(SchemaError::InvalidParameter {
                            task: task.to_owned(),
                            name: name.to_owned(),
                            message: format!("default must be one of {:?}", values),
                        });
                    }
                }
                if definition
                    .values
                    .as_ref()
                    .is_some_and(|values| values.is_empty())
                {
                    return Err(SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "values must not be empty for choice parameters".to_owned(),
                    });
                }
            }
            TaskParameterType::Boolean => {
                if let Some(default) = &definition.default
                    && !default.is_boolean()
                {
                    return Err(SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "default must be a boolean".to_owned(),
                    });
                }
                if definition.values.is_some() {
                    return Err(SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "values is only allowed for choice parameters".to_owned(),
                    });
                }
            }
            TaskParameterType::String => {
                if let Some(default) = &definition.default
                    && !default.is_string()
                {
                    return Err(SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "default must be a string".to_owned(),
                    });
                }
                if definition.values.is_some() {
                    return Err(SchemaError::InvalidParameter {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: "values is only allowed for choice parameters".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TaskParameterType;
    use serde_json::json;

    fn sample_choice() -> BTreeMap<String, TaskParameter> {
        BTreeMap::from([(
            "mode".to_owned(),
            TaskParameter {
                param_type: TaskParameterType::Choice,
                default: Some(json!("fast")),
                values: Some(vec!["fast".to_owned(), "slow".to_owned()]),
            },
        )])
    }

    #[test]
    fn parameter_env_name_uppercases() {
        assert_eq!(parameter_env_name("mode"), "NXR_PARAM_MODE");
        assert_eq!(parameter_env_name("buildTarget"), "NXR_PARAM_BUILDTARGET");
    }

    #[test]
    fn resolve_uses_defaults() {
        let env = resolve_task_parameter_env("demo", &sample_choice()).expect("defaults");
        assert_eq!(env.get("NXR_PARAM_MODE").map(String::as_str), Some("fast"));
    }

    #[test]
    fn resolve_prefers_caller_env() {
        let lookup = |key: &str| {
            if key == "NXR_PARAM_MODE" {
                Some("slow".to_owned())
            } else {
                None
            }
        };
        let env = resolve_task_parameter_env_with("demo", &sample_choice(), lookup)
            .expect("env override");
        assert_eq!(env.get("NXR_PARAM_MODE").map(String::as_str), Some("slow"));
    }

    #[test]
    fn rejects_unknown_choice_value() {
        let lookup = |key: &str| {
            if key == "NXR_PARAM_MODE" {
                Some("invalid".to_owned())
            } else {
                None
            }
        };
        let err =
            resolve_task_parameter_env_with("demo", &sample_choice(), lookup).expect_err("invalid");
        assert!(matches!(err, ParameterError::Invalid { .. }));
    }

    #[test]
    fn layered_prefers_cli_sets_over_env_and_default() {
        let cli = BTreeMap::from([("mode".to_owned(), "slow".to_owned())]);
        let lookup = |key: &str| {
            if key == "NXR_PARAM_MODE" {
                Some("fast".to_owned())
            } else {
                None
            }
        };
        let env = resolve_task_parameter_env_layered("demo", &sample_choice(), &cli, lookup, None)
            .expect("cli");
        assert_eq!(env.get("NXR_PARAM_MODE").map(String::as_str), Some("slow"));
    }

    #[test]
    fn layered_prompt_fills_required_missing() {
        let parameters = BTreeMap::from([(
            "reason".to_owned(),
            TaskParameter {
                param_type: TaskParameterType::String,
                default: None,
                values: None,
            },
        )]);
        let mut prompted = false;
        let mut prompt = |name: &str, _: &TaskParameter| {
            prompted = true;
            assert_eq!(name, "reason");
            Ok("deploy".to_owned())
        };
        let env = resolve_task_parameter_env_layered(
            "demo",
            &parameters,
            &BTreeMap::new(),
            |_| None,
            Some(&mut prompt),
        )
        .expect("prompt");
        assert!(prompted);
        assert_eq!(
            env.get("NXR_PARAM_REASON").map(String::as_str),
            Some("deploy")
        );
    }

    #[test]
    fn layered_fail_closed_without_prompt() {
        let parameters = BTreeMap::from([(
            "reason".to_owned(),
            TaskParameter {
                param_type: TaskParameterType::String,
                default: None,
                values: None,
            },
        )]);
        let err = resolve_task_parameter_env_layered(
            "demo",
            &parameters,
            &BTreeMap::new(),
            |_| None,
            None,
        )
        .expect_err("missing");
        assert!(matches!(err, ParameterError::Missing { .. }));
    }

    #[test]
    fn validate_rejects_parameter_name_collision_after_normalization() {
        let parameters = BTreeMap::from([
            (
                "foo".to_owned(),
                TaskParameter {
                    param_type: TaskParameterType::String,
                    default: None,
                    values: None,
                },
            ),
            (
                "Foo".to_owned(),
                TaskParameter {
                    param_type: TaskParameterType::String,
                    default: None,
                    values: None,
                },
            ),
        ]);
        let err = validate_task_parameters("demo", &parameters).expect_err("collision");
        assert!(matches!(err, SchemaError::InvalidParameter { .. }));
        let message = err.to_string();
        assert!(message.contains("collides"));
        assert!(message.contains("NXR_PARAM_FOO"));
    }

    #[test]
    fn validate_rejects_parameter_name_with_leading_digit() {
        let parameters = BTreeMap::from([(
            "1foo".to_owned(),
            TaskParameter {
                param_type: TaskParameterType::String,
                default: None,
                values: None,
            },
        )]);
        let err = validate_task_parameters("demo", &parameters).expect_err("leading digit");
        assert!(matches!(err, SchemaError::InvalidParameter { .. }));
    }

    #[test]
    fn validate_rejects_empty_choice_values() {
        let parameters = BTreeMap::from([(
            "mode".to_owned(),
            TaskParameter {
                param_type: TaskParameterType::Choice,
                default: None,
                values: Some(vec![]),
            },
        )]);
        let err = validate_task_parameters("demo", &parameters).expect_err("empty values");
        assert!(matches!(err, SchemaError::InvalidParameter { .. }));
    }
}
