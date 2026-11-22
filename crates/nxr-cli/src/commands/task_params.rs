//! CLI `--set` / TTY resolution for typed task parameters.
//!
//! Interactive prompts use dialoguer list/input widgets (not fullscreen fuzzy
//! pickers) so tmux/zellij sessions avoid alternate-screen conflicts.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal};

use nxr_task::{
    ExecutionPlan, ParameterError, TaskDocument, TaskParameter, expand_matrix_tasks,
    parameter_env_name,
};

/// Input collection settings retained across watch generations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskInputOptions {
    pub file: Option<std::path::PathBuf>,
    pub interactive: Option<bool>,
    pub context_override: Option<String>,
    pub invocation_directory: Option<std::path::PathBuf>,
}

/// True when `TMUX` or `ZELLIJ` is set (terminal multiplexer session).
#[must_use]
pub fn mux_session_active() -> bool {
    mux_active_from_env_os(std::env::var_os("TMUX"), std::env::var_os("ZELLIJ"))
}

fn mux_active_from_env_os(
    tmux: Option<std::ffi::OsString>,
    zellij: Option<std::ffi::OsString>,
) -> bool {
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
    if mux_session_active() {
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
#[cfg(test)]
pub fn resolve_plan_parameters(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    cli_sets: &BTreeMap<String, String>,
    options: &TaskInputOptions,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, ParameterError> {
    Ok(resolve_plan_inputs(document, plan, cli_sets, options)?
        .into_iter()
        .map(|(node, inputs)| (node, inputs.environment()))
        .collect())
}

/// Resolve values once per node; callers persist only `ResolvedInputs::provenance()`.
pub struct PartialPlanInputs {
    pub resolved: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    pub missing: Vec<(String, String, TaskParameter)>,
}

pub fn collect_plan_inputs(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    cli_sets: &BTreeMap<String, String>,
    options: &TaskInputOptions,
) -> Result<PartialPlanInputs, ParameterError> {
    let mut options = options.clone();
    options.interactive = Some(false);
    let (resolved, missing) =
        resolve_plan_inputs_internal(document, plan, cli_sets, &options, true)?;
    Ok(PartialPlanInputs {
        resolved: resolved
            .into_iter()
            .map(|(node, inputs)| {
                (
                    node,
                    inputs
                        .values
                        .into_iter()
                        .map(|(name, input)| (name, input.value))
                        .collect(),
                )
            })
            .collect(),
        missing,
    })
}

pub fn resolve_plan_inputs(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    cli_sets: &BTreeMap<String, String>,
    options: &TaskInputOptions,
) -> Result<BTreeMap<String, nxr_task::ResolvedInputs>, ParameterError> {
    resolve_plan_inputs_internal(document, plan, cli_sets, options, false).map(|(inputs, _)| inputs)
}

#[allow(clippy::type_complexity)]
fn resolve_plan_inputs_internal(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    cli_sets: &BTreeMap<String, String>,
    options: &TaskInputOptions,
    allow_missing: bool,
) -> Result<
    (
        BTreeMap<String, nxr_task::ResolvedInputs>,
        Vec<(String, String, TaskParameter)>,
    ),
    ParameterError,
> {
    let invalid = |name: &str, message: String| ParameterError::Invalid {
        task: plan.root.clone(),
        name: name.to_owned(),
        message,
    };
    let expansion = expand_matrix_tasks(&document.tasks)
        .map_err(|error| invalid("matrix", error.to_string()))?;
    let file_values = if let Some(path) = &options.file {
        let bytes = std::fs::read(path).map_err(|error| invalid("inputs", error.to_string()))?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("inputs", "invalid JSON values document".to_owned()))?;
        nxr_task::parse_input_values(&json)?.inputs
    } else {
        BTreeMap::new()
    };
    let tasks: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .filter_map(|node| {
            expansion
                .tasks
                .get(&node.id)
                .or_else(|| document.tasks.get(&node.id))
                .map(|definition| (node.id.clone(), definition))
        })
        .collect();
    let known_names: BTreeSet<_> = tasks
        .values()
        .flat_map(|definition| definition.parameters.keys().cloned())
        .collect();
    for (node, values) in &file_values {
        let definition = tasks.get(node).ok_or_else(|| {
            invalid(
                "inputs",
                format!("input document names node {node} outside this execution plan"),
            )
        })?;
        for name in values.keys() {
            if !definition.parameters.contains_key(name) {
                return Err(ParameterError::UnknownSet {
                    name: format!("{node}.{name}"),
                });
            }
        }
    }
    for name in cli_sets.keys() {
        if known_names.contains(name) {
            let mut declarations = tasks
                .values()
                .filter_map(|definition| definition.parameters.get(name));
            if let Some(first) = declarations.next()
                && declarations.any(|other| other != first)
            {
                return Err(invalid(
                    name,
                    "ambiguous unqualified override; use NODE.PARAMETER=value".to_owned(),
                ));
            }
        } else if !tasks.iter().any(|(node, definition)| {
            name.strip_prefix(&format!("{node}."))
                .is_some_and(|parameter| definition.parameters.contains_key(parameter))
        }) {
            return Err(ParameterError::UnknownSet { name: name.clone() });
        }
    }
    let interactive = options
        .interactive
        .unwrap_or_else(parameter_prompts_interactive);
    if interactive && !parameter_prompts_interactive() {
        return Err(invalid(
            "interactive",
            "interactive input requires stdin and stderr TTYs".to_owned(),
        ));
    }
    let base = options
        .invocation_directory
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|error| invalid("inputs", error.to_string()))?;
    let mut output = BTreeMap::new();
    let mut missing_inputs = Vec::new();
    for (node, definition) in tasks {
        let mut scoped = BTreeMap::new();
        let mut cli = BTreeMap::new();
        let mut environment = BTreeMap::new();
        let mut context = BTreeMap::new();
        let context_environment = options
            .context_override
            .as_ref()
            .or(definition.context.as_ref())
            .and_then(|name| document.contexts.get(name))
            .and_then(|context| context.environment.as_ref());
        let file = file_values.get(&node);
        for (name, parameter) in &definition.parameters {
            if let Some(raw) = cli_sets
                .get(&format!("{node}.{name}"))
                .filter(|value| document.schema_version >= 3 || !value.is_empty())
            {
                scoped.insert(
                    name.clone(),
                    nxr_task::parse_parameter_text(&node, name, parameter, raw)?,
                );
            } else if let Some(raw) = cli_sets
                .get(name)
                .filter(|value| document.schema_version >= 3 || !value.is_empty())
            {
                cli.insert(
                    name.clone(),
                    nxr_task::parse_parameter_text(&node, name, parameter, raw)?,
                );
            } else if !file.is_some_and(|values| values.contains_key(name)) {
                if let Some(raw) = context_environment
                    .and_then(|environment| environment.set.get(&parameter_env_name(name)))
                {
                    context.insert(
                        name.clone(),
                        nxr_task::parse_parameter_text(&node, name, parameter, raw)?,
                    );
                } else if let Ok(raw) = std::env::var(parameter_env_name(name))
                    && !raw.is_empty()
                {
                    environment.insert(
                        name.clone(),
                        nxr_task::parse_parameter_text(&node, name, parameter, &raw)?,
                    );
                }
            }
        }
        let mut answers = BTreeMap::new();
        let mut remaining_parameters = definition.parameters.clone();
        loop {
            let resolved = nxr_task::resolve_node_inputs(
                &node,
                &remaining_parameters,
                nxr_task::InputLayers {
                    scoped_cli: Some(&scoped),
                    cli: Some(&cli),
                    file,
                    context: Some(&context),
                    environment: Some(&environment),
                    answers: Some(&answers),
                    base_directory: Some(&base),
                },
            );
            match resolved {
                Ok(inputs) => {
                    output.insert(node.clone(), inputs);
                    break;
                }
                Err(ParameterError::Missing { name, .. }) if interactive => {
                    let parameter = &definition.parameters[&name];
                    let answer = prompt_parameter(&node, &name, parameter)?;
                    answers.insert(
                        name.clone(),
                        nxr_task::parse_parameter_text(&node, &name, parameter, &answer)?,
                    );
                }
                Err(ParameterError::Missing { name, .. }) if allow_missing => {
                    if let Some(declaration) = remaining_parameters.remove(&name) {
                        missing_inputs.push((node.clone(), name, declaration));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok((output, missing_inputs))
}

fn prompt_parameter(
    task: &str,
    name: &str,
    definition: &TaskParameter,
) -> Result<String, ParameterError> {
    use nxr_task::interaction::InteractionFrontend;
    let request = nxr_task::interaction::InputRequest {
        request_id: format!("{task}.{name}"),
        node_id: task.to_owned(),
        parameter_name: name.to_owned(),
        declaration: definition.clone(),
        observation_digest: None,
        revision: 0,
    };
    super::interaction::TerminalFrontend
        .answer(&request)
        .map_err(|error| ParameterError::Prompt {
            task: task.to_owned(),
            name: name.to_owned(),
            message: error.to_string(),
        })?
        .map(|value| nxr_task::input_environment_value(&value))
        .ok_or_else(|| ParameterError::Prompt {
            task: task.to_owned(),
            name: name.to_owned(),
            message: "input cancelled".to_owned(),
        })
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
    fn scoped_fixture() -> (TaskDocument, ExecutionPlan) {
        let document = nxr_task::parse_task_document(&serde_json::json!({
            "schema_version": 2,
            "tasks": {
                "prepare": {"app":"prepare", "parameters":{"pleroma_test_target":{"type":"string","default":"linux"}}},
                "verify": {"app":"verify", "dependsOn":["prepare"], "parameters":{"pleroma_test_target":{"type":"string","default":"darwin"}}}
            }
        })).expect("document");
        let plan = nxr_task::build_execution_plan(
            &document.tasks,
            "verify",
            nxr_task::FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        (document, plan)
    }

    #[test]
    fn different_node_defaults_do_not_overwrite_each_other() {
        let (document, plan) = scoped_fixture();
        let values = resolve_plan_parameters(
            &document,
            &plan,
            &BTreeMap::new(),
            &TaskInputOptions {
                interactive: Some(false),
                ..Default::default()
            },
        )
        .expect("resolve");
        assert_eq!(values["prepare"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "linux");
        assert_eq!(values["verify"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "darwin");
    }

    #[test]
    fn scoped_overrides_are_isolated_and_ambiguous_unqualified_values_fail() {
        let (document, plan) = scoped_fixture();
        let options = TaskInputOptions {
            interactive: Some(false),
            ..Default::default()
        };
        let cli = BTreeMap::from([(
            "prepare.pleroma_test_target".to_owned(),
            "aarch64".to_owned(),
        )]);
        let values = resolve_plan_parameters(&document, &plan, &cli, &options).expect("scoped");
        assert_eq!(
            values["prepare"]["NXR_PARAM_PLEROMA_TEST_TARGET"],
            "aarch64"
        );
        assert_eq!(values["verify"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "darwin");
        let ambiguous = BTreeMap::from([("pleroma_test_target".to_owned(), "aarch64".to_owned())]);
        assert!(
            resolve_plan_parameters(&document, &plan, &ambiguous, &options)
                .expect_err("ambiguity")
                .to_string()
                .contains("ambiguous")
        );
    }

    #[test]
    fn file_values_are_node_scoped_and_cli_wins() {
        let (document, plan) = scoped_fixture();
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("inputs.json");
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"schema_version":"nxr-input-values-v1","inputs":{"prepare":{"pleroma_test_target":"file"}}})).expect("json")).expect("file");
        let options = TaskInputOptions {
            file: Some(path),
            interactive: Some(false),
            ..Default::default()
        };
        let values =
            resolve_plan_parameters(&document, &plan, &BTreeMap::new(), &options).expect("file");
        assert_eq!(values["prepare"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "file");
        assert_eq!(values["verify"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "darwin");
        let cli = BTreeMap::from([("prepare.pleroma_test_target".to_owned(), "cli".to_owned())]);
        let values = resolve_plan_parameters(&document, &plan, &cli, &options).expect("cli");
        assert_eq!(values["prepare"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "cli");
    }
    #[test]
    fn explicit_v3_empty_string_and_context_precedence_survive_resolution() {
        let document = nxr_task::parse_task_document(&serde_json::json!({
            "schema_version": 3,
            "contexts": {"declared":{"environment":{"mode":"inherit","set":{"NXR_PARAM_PLEROMA_TEST_TARGET":"context"}}}},
            "tasks":{"prepare":{"app":"prepare","context":"declared","parameters":{"pleroma_test_target":{"type":"string","default":"default"}}}}
        })).expect("document");
        let plan = nxr_task::build_execution_plan(
            &document.tasks,
            "prepare",
            nxr_task::FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        let options = TaskInputOptions {
            interactive: Some(false),
            ..Default::default()
        };
        let values =
            resolve_plan_inputs(&document, &plan, &BTreeMap::new(), &options).expect("context");
        assert_eq!(
            values["prepare"].environment()["NXR_PARAM_PLEROMA_TEST_TARGET"],
            "context"
        );
        assert_eq!(
            values["prepare"].provenance()["pleroma_test_target"],
            nxr_task::InputProvenance::Context
        );
        let cli = BTreeMap::from([("pleroma_test_target".to_owned(), String::new())]);
        let values =
            resolve_plan_parameters(&document, &plan, &cli, &options).expect("explicit empty");
        assert_eq!(values["prepare"]["NXR_PARAM_PLEROMA_TEST_TARGET"], "");
    }
}
