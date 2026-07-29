//! Typed task parameter resolution at spawn time.
//!
//! Parameter **values** are injected as `NXR_PARAM_<NAME>` environment variables
//! when a task child is spawned. Plans and events carry parameter names only.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::schema::{SchemaError, TaskParameter, TaskParameterType};

/// Prefix for spawn-time task parameter environment variables.
pub const PARAM_ENV_PREFIX: &str = "NXR_PARAM_";

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
    /// A parameter has no default and was not provided in the caller environment.
    #[error(
        "task {task}: required parameter {name} is unset (set {env_name} or declare a default)"
    )]
    Missing {
        task: String,
        name: String,
        env_name: String,
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

/// Like [`resolve_task_parameter_env`] with an explicit caller-env lookup hook.
pub fn resolve_task_parameter_env_with(
    task: &str,
    parameters: &BTreeMap<String, TaskParameter>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    let mut env = BTreeMap::new();
    for (name, definition) in parameters {
        let env_name = parameter_env_name(name);
        let raw = lookup(&env_name);
        let value = match raw {
            Some(value) if !value.is_empty() => value,
            _ => match definition.default.as_ref() {
                Some(default) => json_default_to_string(task, name, definition, default)?,
                None => {
                    return Err(ParameterError::Missing {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        env_name,
                    });
                }
            },
        };
        let normalized = normalize_parameter_value(task, name, definition, &value)?;
        env.insert(env_name, normalized);
    }
    Ok(env)
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
    for (name, definition) in parameters {
        if name.trim().is_empty() {
            return Err(SchemaError::InvalidParameter {
                task: task.to_owned(),
                name: name.to_owned(),
                message: "name must not be empty".to_owned(),
            });
        }
        if !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(SchemaError::InvalidParameter {
                task: task.to_owned(),
                name: name.to_owned(),
                message: "name must be alphanumeric or underscore".to_owned(),
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
