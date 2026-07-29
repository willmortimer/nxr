//! Dynamic completion candidate protocol.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::ValueEnum;
use nxr_core::App;
use nxr_task::{TaskParameter, TaskParameterType};

use crate::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};

/// Maximum time to wait for a cold discovery during interactive completion.
///
/// When discovery exceeds this budget, completion falls back to static command
/// names only (empty app candidates).
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Dynamic completion targets invoked through the hidden `__complete` command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CompleteTarget {
    /// Flake app names for the current workspace.
    Apps,
    /// Task names (and aliases) from optional `nxr` metadata.
    Tasks,
    /// `packages.<system>.*` leaf names.
    Packages,
    /// `checks.<system>.*` leaf names.
    Checks,
    /// `devShells.<system>.*` leaf names.
    Shells,
    /// Project namespaces from `nxr.projects.json`.
    Namespaces,
    /// Categories declared on apps/tasks.
    Categories,
    /// Typed parameter names for a task (`task-parameters <TASK>`).
    TaskParameters,
    /// Typed parameter value candidates (`task-parameter-values <TASK> <PARAMETER>`).
    TaskParameterValues,
}

/// Discover app candidates for shell completion.
///
/// Uses the discovery cache when possible. Cold misses should evaluate apps
/// and, when available, the lightweight `nxr` task document so `discoveryInputs`
/// enter the first cache entry. Task discovery remains best-effort so optional
/// metadata cannot erase ordinary app completions. On a cache miss, discovery
/// runs in a background thread and is abandoned after [`DISCOVERY_TIMEOUT`],
/// returning an empty list so shells never block on slow Nix evaluation.
pub fn discover_app_candidates<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Vec<App>
where
    F: FnOnce() -> Result<WorkspaceDiscovery, E> + Send + 'static,
    E: Send + 'static,
{
    // Cache hits return quickly inside discover_workspace_with_cache; cold
    // evaluation is abandoned after DISCOVERY_TIMEOUT.
    let context = context.clone();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let result = discover_workspace_with_cache(&context, options, discover);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(DISCOVERY_TIMEOUT) {
        Ok(Ok(workspace)) => workspace.apps,
        _ => Vec::new(),
    }
}

/// Write one candidate per line as `name` or `name<TAB>description`.
///
/// # Errors
///
/// Returns an I/O error when writing fails.
pub fn write_app_candidates(apps: &[App], writer: &mut dyn Write) -> io::Result<()> {
    for app in apps {
        match &app.description {
            Some(description) => writeln!(writer, "{}\t{description}", app.name)?,
            None => writeln!(writer, "{}", app.name)?,
        }
    }
    Ok(())
}

/// Build sorted parameter-name candidates with type descriptions for completion.
#[must_use]
pub fn task_parameter_name_candidates(
    parameters: &BTreeMap<String, TaskParameter>,
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = parameters
        .iter()
        .map(|(name, definition)| (name.clone(), parameter_description(definition)))
        .collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

/// Build value candidates for a typed task parameter.
#[must_use]
pub fn task_parameter_value_candidates(parameter: &TaskParameter) -> Vec<String> {
    match parameter.param_type {
        TaskParameterType::Boolean => vec!["false".to_owned(), "true".to_owned()],
        TaskParameterType::Choice => {
            let mut values = parameter.values.clone().unwrap_or_default();
            values.sort();
            values
        }
        TaskParameterType::String => parameter
            .default
            .as_ref()
            .and_then(|value| value.as_str())
            .map(|default| vec![default.to_owned()])
            .unwrap_or_default(),
    }
}

/// Write one parameter name per line as `name<TAB>description`.
///
/// # Errors
///
/// Returns an I/O error when writing fails.
pub fn write_task_parameter_candidates(
    parameters: &BTreeMap<String, TaskParameter>,
    writer: &mut dyn Write,
) -> io::Result<()> {
    for (name, description) in task_parameter_name_candidates(parameters) {
        writeln!(writer, "{name}\t{description}")?;
    }
    Ok(())
}

/// Write one parameter value candidate per line.
///
/// # Errors
///
/// Returns an I/O error when writing fails.
pub fn write_task_parameter_value_candidates(
    parameter: &TaskParameter,
    writer: &mut dyn Write,
) -> io::Result<()> {
    for value in task_parameter_value_candidates(parameter) {
        writeln!(writer, "{value}")?;
    }
    Ok(())
}

fn parameter_description(parameter: &TaskParameter) -> String {
    match parameter.param_type {
        TaskParameterType::String => {
            let mut description = "string".to_owned();
            if let Some(default) = parameter.default.as_ref().and_then(|value| value.as_str()) {
                description.push_str(&format!(" (default: {default})"));
            }
            description
        }
        TaskParameterType::Boolean => {
            let default = parameter
                .default
                .as_ref()
                .and_then(|value| value.as_bool())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "false".to_owned());
            format!("boolean (default: {default})")
        }
        TaskParameterType::Choice => {
            if let Some(values) = &parameter.values {
                format!("choice: {}", values.join(", "))
            } else {
                "choice".to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::thread;
    use std::time::Duration;

    use clap::ValueEnum;
    use nxr_task::{TaskParameter, TaskParameterType};
    use serde_json::json;

    use super::{
        CompleteTarget, DISCOVERY_TIMEOUT, discover_app_candidates, task_parameter_name_candidates,
        task_parameter_value_candidates, write_app_candidates, write_task_parameter_candidates,
        write_task_parameter_value_candidates,
    };
    use crate::cache::{DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery};
    use nxr_core::App;

    #[test]
    fn complete_target_variants_match_cli_spellings() {
        assert_eq!(
            CompleteTarget::value_variants(),
            &[
                CompleteTarget::Apps,
                CompleteTarget::Tasks,
                CompleteTarget::Packages,
                CompleteTarget::Checks,
                CompleteTarget::Shells,
                CompleteTarget::Namespaces,
                CompleteTarget::Categories,
                CompleteTarget::TaskParameters,
                CompleteTarget::TaskParameterValues,
            ]
        );
        assert_eq!(
            CompleteTarget::Apps.to_possible_value().unwrap().get_name(),
            "apps"
        );
        assert_eq!(
            CompleteTarget::Tasks
                .to_possible_value()
                .unwrap()
                .get_name(),
            "tasks"
        );
        assert_eq!(
            CompleteTarget::Packages
                .to_possible_value()
                .unwrap()
                .get_name(),
            "packages"
        );
        assert_eq!(
            CompleteTarget::Checks
                .to_possible_value()
                .unwrap()
                .get_name(),
            "checks"
        );
        assert_eq!(
            CompleteTarget::Shells
                .to_possible_value()
                .unwrap()
                .get_name(),
            "shells"
        );
        assert_eq!(
            CompleteTarget::Namespaces
                .to_possible_value()
                .unwrap()
                .get_name(),
            "namespaces"
        );
        assert_eq!(
            CompleteTarget::Categories
                .to_possible_value()
                .unwrap()
                .get_name(),
            "categories"
        );
        assert_eq!(
            CompleteTarget::TaskParameters
                .to_possible_value()
                .unwrap()
                .get_name(),
            "task-parameters"
        );
        assert_eq!(
            CompleteTarget::TaskParameterValues
                .to_possible_value()
                .unwrap()
                .get_name(),
            "task-parameter-values"
        );
    }

    #[test]
    fn discovery_timeout_is_short() {
        assert!(DISCOVERY_TIMEOUT <= Duration::from_secs(1));
    }

    #[test]
    fn write_app_candidates_uses_tab_separated_descriptions() {
        let apps = vec![
            App {
                name: "lint".to_owned(),
                attr_path: "apps.aarch64-darwin.lint".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Run static analysis".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "test".to_owned(),
                attr_path: "apps.aarch64-darwin.test".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ];

        let mut cursor = Cursor::new(Vec::new());
        write_app_candidates(&apps, &mut cursor).expect("write");
        let output = String::from_utf8(cursor.into_inner()).expect("utf8");
        assert!(output.contains("lint\tRun static analysis\n"));
        assert!(output.contains("test\n"));
    }

    #[test]
    fn discover_app_candidates_returns_empty_on_slow_discover() {
        let context = DiscoveryContext::new("github:owner/repo", None, "aarch64-darwin");
        let apps = discover_app_candidates(&context, DiscoveryCacheOptions::normal(), || {
            thread::sleep(DISCOVERY_TIMEOUT + Duration::from_millis(200));
            Ok::<WorkspaceDiscovery, ()>(WorkspaceDiscovery {
                apps: Vec::new(),
                tasks: None,
                ..Default::default()
            })
        });
        assert!(apps.is_empty());
    }

    fn sample_parameters() -> BTreeMap<String, TaskParameter> {
        BTreeMap::from([
            (
                "mode".to_owned(),
                TaskParameter {
                    param_type: TaskParameterType::Choice,
                    default: Some(json!("fast")),
                    values: Some(vec!["fast".to_owned(), "slow".to_owned()]),
                },
            ),
            (
                "verbose".to_owned(),
                TaskParameter {
                    param_type: TaskParameterType::Boolean,
                    default: Some(json!(false)),
                    values: None,
                },
            ),
            (
                "label".to_owned(),
                TaskParameter {
                    param_type: TaskParameterType::String,
                    default: Some(json!("fixture")),
                    values: None,
                },
            ),
        ])
    }

    #[test]
    fn task_parameter_name_candidates_include_type_descriptions() {
        let parameters = sample_parameters();
        let candidates = task_parameter_name_candidates(&parameters);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].0, "label");
        assert!(candidates[0].1.contains("string"));
        assert!(candidates[0].1.contains("fixture"));
        assert_eq!(candidates[1].0, "mode");
        assert!(candidates[1].1.contains("choice"));
        assert!(candidates[1].1.contains("fast"));
        assert_eq!(candidates[2].0, "verbose");
        assert!(candidates[2].1.contains("boolean"));
    }

    #[test]
    fn task_parameter_value_candidates_follow_declared_types() {
        let parameters = sample_parameters();
        assert_eq!(
            task_parameter_value_candidates(&parameters["mode"]),
            vec!["fast".to_owned(), "slow".to_owned()]
        );
        assert_eq!(
            task_parameter_value_candidates(&parameters["verbose"]),
            vec!["false".to_owned(), "true".to_owned()]
        );
        assert_eq!(
            task_parameter_value_candidates(&parameters["label"]),
            vec!["fixture".to_owned()]
        );
    }

    #[test]
    fn write_task_parameter_candidates_use_tab_separated_descriptions() {
        let mut cursor = Cursor::new(Vec::new());
        write_task_parameter_candidates(&sample_parameters(), &mut cursor).expect("write");
        let output = String::from_utf8(cursor.into_inner()).expect("utf8");
        assert!(output.contains("mode\tchoice: fast, slow\n"));
        assert!(output.contains("verbose\tboolean (default: false)\n"));
    }

    #[test]
    fn write_task_parameter_value_candidates_emit_one_per_line() {
        let parameters = sample_parameters();
        let mut cursor = Cursor::new(Vec::new());
        write_task_parameter_value_candidates(&parameters["mode"], &mut cursor).expect("write");
        let output = String::from_utf8(cursor.into_inner()).expect("utf8");
        assert_eq!(output, "fast\nslow\n");
    }
}
