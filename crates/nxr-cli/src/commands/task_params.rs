//! CLI `--set` / TTY resolution for typed task parameters.
//!
//! Interactive prompts use dialoguer list/input widgets (not fullscreen fuzzy
//! pickers) so tmux/zellij sessions avoid alternate-screen conflicts.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};

use dialoguer::{Confirm, Input, Select};
use nxr_task::{
    ExecutionPlan, ParameterError, TaskDocument, TaskParameter, TaskParameterType,
    expand_matrix_tasks, parameter_env_name, resolve_task_parameter_env_layered,
};

/// True when `TMUX` or `ZELLIJ` is set (terminal multiplexer session).
fn terminal_mux_active() -> bool {
    mux_active_from_env_os(std::env::var_os("TMUX"), std::env::var_os("ZELLIJ"))
}

fn mux_active_from_env_os(tmux: Option<std::ffi::OsString>, zellij: Option<std::ffi::OsString>) -> bool {
    tmux.is_some() || zellij.is_some()
}

/// Whether CLI may run dialoguer prompts for typed parameters.
///
/// Requires stdin and stderr TTYs. Inside tmux/zellij without both, degrades to
/// fail-closed `--set` / `NXR_PARAM_*` / defaults (no hung reads).
fn parameter_prompts_interactive() -> bool {
    parameter_prompts_interactive_tty(io::stdin().is_terminal(), io::stderr().is_terminal())
}

fn parameter_prompts_interactive_tty(stdin_tty: bool, stderr_tty: bool) -> bool {
    if stdin_tty && stderr_tty {
        return true;
    }
    if terminal_mux_active() {
        return false;
    }
    false
}

/// Parse a single `--set name=value` argument.
///
/// # Errors
///
/// Returns a usage message when `=` is missing or the name is empty.
pub fn parse_param_set(raw: &str) -> Result<(String, String), String> {
    let (name, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("--set requires NAME=VALUE (got {raw})"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err("--set name must not be empty".to_owned());
    }
    Ok((name.to_owned(), value.to_owned()))
}

/// Parse repeatable `--set` values into a map (later entries win).
///
/// # Errors
///
/// Returns the first malformed `--set` message.
pub fn parse_param_sets(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut sets = BTreeMap::new();
    for entry in raw {
        let (name, value) = parse_param_set(entry)?;
        sets.insert(name, value);
    }
    Ok(sets)
}

/// Resolve parameters for every node in `plan` into `NXR_PARAM_*` assignments.
///
/// Lookup order per parameter: `--set` → caller `NXR_PARAM_*` → default → TTY
/// prompt (when stdin/stderr are TTYs) → fail-closed [`ParameterError::Missing`].
///
/// # Errors
///
/// Returns [`ParameterError`] for unknown `--set` keys, invalid values, cancelled
/// prompts, or missing required parameters in non-interactive mode.
pub fn resolve_plan_parameters(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    cli_sets: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ParameterError> {
    let expansion =
        expand_matrix_tasks(&document.tasks).map_err(|error| ParameterError::Invalid {
            task: plan.root.clone(),
            name: "matrix".to_owned(),
            message: error.to_string(),
        })?;

    let mut known_names = BTreeSet::new();
    let mut tasks_with_params: BTreeMap<String, &nxr_task::TaskDefinition> = BTreeMap::new();
    for node in &plan.nodes {
        let definition = expansion
            .tasks
            .get(&node.id)
            .or_else(|| document.tasks.get(&node.id));
        let Some(definition) = definition else {
            continue;
        };
        if definition.parameters.is_empty() {
            continue;
        }
        for name in definition.parameters.keys() {
            known_names.insert(name.clone());
        }
        tasks_with_params.insert(node.id.clone(), definition);
    }

    for name in cli_sets.keys() {
        if !known_names.contains(name) {
            return Err(ParameterError::UnknownSet { name: name.clone() });
        }
    }

    let mut merged = BTreeMap::new();
    if tasks_with_params.is_empty() {
        return Ok(merged);
    }

    let interactive = parameter_prompts_interactive();

    for (task_id, definition) in tasks_with_params {
        let task_id_for_prompt = task_id.clone();
        let mut prompt_fn;
        let prompt: Option<&mut dyn FnMut(&str, &TaskParameter) -> Result<String, ParameterError>> =
            if interactive {
                prompt_fn = move |name: &str, definition: &TaskParameter| {
                    prompt_parameter(&task_id_for_prompt, name, definition)
                };
                Some(&mut prompt_fn)
            } else {
                None
            };

        let env = resolve_task_parameter_env_layered(
            &task_id,
            &definition.parameters,
            cli_sets,
            |env_name| {
                std::env::var(env_name)
                    .ok()
                    .filter(|value| !value.is_empty())
            },
            prompt,
        )?;
        merged.extend(env);
    }

    Ok(merged)
}

fn prompt_parameter(
    task: &str,
    name: &str,
    definition: &TaskParameter,
) -> Result<String, ParameterError> {
    let prompt = format!("task {task}: parameter {name}");
    let result = match definition.param_type {
        TaskParameterType::Choice => {
            let values = definition
                .values
                .as_ref()
                .ok_or_else(|| ParameterError::Invalid {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    message: "choice parameter has no values".to_owned(),
                })?;
            // List-style Select (not fuzzy/fullscreen) for tmux/zellij compatibility.
            let selection = Select::new()
                .with_prompt(&prompt)
                .items(values)
                .default(0)
                .interact_opt()
                .map_err(|error| ParameterError::Prompt {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    message: error.to_string(),
                })?;
            match selection {
                Some(index) => Ok(values[index].clone()),
                None => Err(ParameterError::Prompt {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    message: "selection cancelled".to_owned(),
                }),
            }
        }
        TaskParameterType::Boolean => {
            let confirmed = Confirm::new()
                .with_prompt(&prompt)
                .default(false)
                .interact_opt()
                .map_err(|error| ParameterError::Prompt {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    message: error.to_string(),
                })?;
            match confirmed {
                Some(true) => Ok("true".to_owned()),
                Some(false) => Ok("false".to_owned()),
                None => Err(ParameterError::Prompt {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    message: "confirmation cancelled".to_owned(),
                }),
            }
        }
        TaskParameterType::String => {
            let value: String =
                Input::new()
                    .with_prompt(&prompt)
                    .interact_text()
                    .map_err(|error| ParameterError::Prompt {
                        task: task.to_owned(),
                        name: name.to_owned(),
                        message: error.to_string(),
                    })?;
            if value.is_empty() {
                Err(ParameterError::Missing {
                    task: task.to_owned(),
                    name: name.to_owned(),
                    env_name: parameter_env_name(name),
                })
            } else {
                Ok(value)
            }
        }
    };

    let _ = io::stderr().flush();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_param_set_splits_once() {
        let (name, value) = parse_param_set("reason=a=b").expect("parse");
        assert_eq!(name, "reason");
        assert_eq!(value, "a=b");
    }

    #[test]
    fn parse_param_set_rejects_missing_equals() {
        assert!(parse_param_set("reason").is_err());
    }

    #[test]
    fn terminal_mux_active_from_env() {
        assert!(mux_active_from_env_os(
            Some(std::ffi::OsString::from("/tmp/tmux")),
            None,
        ));
        assert!(mux_active_from_env_os(
            None,
            Some(std::ffi::OsString::from("1")),
        ));
        assert!(!mux_active_from_env_os(None, None));
    }

    #[test]
    fn parameter_prompts_require_both_ttys() {
        assert!(parameter_prompts_interactive_tty(true, true));
        assert!(!parameter_prompts_interactive_tty(true, false));
        assert!(!parameter_prompts_interactive_tty(false, true));
        assert!(!parameter_prompts_interactive_tty(false, false));
    }
}
