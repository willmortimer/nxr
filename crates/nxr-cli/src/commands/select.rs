//! Interactive fuzzy selector for flake apps and tasks.

use std::io::{self, IsTerminal};

use dialoguer::FuzzySelect;
use nxr_core::App;
use nxr_core::diagnostics::exit;
use nxr_task::TaskDefinition;

use crate::commands::common::{DiscoverRequest, PrepareError, WorkspaceSnapshot};

/// Errors while running the interactive selector.
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    #[error("interactive selection requires a terminal (stdin and stdout must be TTYs)")]
    NoTty,
    #[error("no apps or tasks discovered for the selected flake")]
    NoTargets,
    #[error("no apps or tasks match the filter")]
    NoMatches,
    #[error("selection cancelled")]
    Cancelled,
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error("interactive selector failed: {0}")]
    Prompt(#[from] dialoguer::Error),
}

impl SelectError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::NoTty | Self::NoTargets | Self::NoMatches | Self::Cancelled | Self::Prompt(_) => {
                exit::USAGE
            }
        }
    }
}

/// Selected catalog entry after an interactive pick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectedTarget {
    App(String),
    Task(String),
}

#[derive(Clone, Debug)]
struct CatalogChoice {
    target: SelectedTarget,
    label: String,
    haystack: String,
}

/// Discover apps and tasks and prompt the user to pick one.
///
/// Requires stdin and stdout TTYs. Non-TTY callers must use structured
/// invocation (`nxr run` / `nxr task`) instead of prompting.
///
/// `filter` is an optional case-insensitive substring applied before the
/// interactive list (operators can still type to narrow further).
///
/// # Errors
///
/// Returns [`SelectError`] when the environment is non-interactive, discovery
/// fails, no targets exist, nothing matches the filter, or the user cancels.
pub fn pick_target(
    request: DiscoverRequest<'_>,
    filter: Option<&str>,
) -> Result<SelectedTarget, SelectError> {
    ensure_interactive_terminal()?;

    let choices = discover_choices(request)?;
    if choices.is_empty() {
        return Err(SelectError::NoTargets);
    }

    let filtered = apply_substring_filter(&choices, filter);
    if filtered.is_empty() {
        return Err(SelectError::NoMatches);
    }

    let labels = filtered
        .iter()
        .map(|choice| choice.label.clone())
        .collect::<Vec<_>>();
    let selection = FuzzySelect::new()
        .with_prompt("Select app or task")
        .items(&labels)
        .default(0)
        .interact_opt()
        .map_err(SelectError::Prompt)?;

    let Some(index) = selection else {
        return Err(SelectError::Cancelled);
    };

    Ok(filtered[index].target.clone())
}

fn discover_choices(request: DiscoverRequest<'_>) -> Result<Vec<CatalogChoice>, SelectError> {
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        true,
        request.nix_flags,
    )?;
    let mut choices = Vec::new();
    for app in snapshot.apps.values() {
        choices.push(app_choice(app));
    }
    if let Some(document) = &snapshot.tasks {
        for (name, task) in &document.tasks {
            choices.push(task_choice(name, task));
        }
    }
    choices.sort_by(|left, right| left.haystack.cmp(&right.haystack));
    Ok(choices)
}

fn apply_substring_filter(choices: &[CatalogChoice], filter: Option<&str>) -> Vec<CatalogChoice> {
    let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return choices.to_vec();
    };
    let needle = filter.to_ascii_lowercase();
    choices
        .iter()
        .filter(|choice| choice.haystack.contains(&needle))
        .cloned()
        .collect()
}

fn ensure_interactive_terminal() -> Result<(), SelectError> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(SelectError::NoTty)
    }
}

fn app_choice(app: &App) -> CatalogChoice {
    let description = app.description.as_deref().unwrap_or("");
    let label = format_entry_label("app", &app.name, app.description.as_deref());
    CatalogChoice {
        target: SelectedTarget::App(app.name.clone()),
        haystack: format!("app {} {description}", app.name).to_ascii_lowercase(),
        label,
    }
}

fn task_choice(name: &str, task: &TaskDefinition) -> CatalogChoice {
    let description = task.description.as_deref().unwrap_or("");
    let label = format_entry_label("task", name, task.description.as_deref());
    CatalogChoice {
        target: SelectedTarget::Task(name.to_owned()),
        haystack: format!("task {name} {description}").to_ascii_lowercase(),
        label,
    }
}

fn format_entry_label(kind: &str, name: &str, description: Option<&str>) -> String {
    let name = nxr_core::sanitize_terminal_text(name);
    match description.filter(|text| !text.is_empty()) {
        Some(description) => format!(
            "{kind:<4} {:<16} {}",
            name,
            nxr_core::sanitize_terminal_text(description)
        ),
        None => format!("{kind:<4} {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogChoice, SelectedTarget, apply_substring_filter, format_entry_label};
    use nxr_core::App;
    use std::collections::BTreeMap;

    #[test]
    fn format_entry_label_includes_kind_and_description() {
        let label = format_entry_label("app", "test", Some("Run the test suite"));
        assert!(label.contains("app"));
        assert!(label.contains("test"));
        assert!(label.contains("Run the test suite"));
    }

    #[test]
    fn substring_filter_is_case_insensitive() {
        let choices = vec![
            CatalogChoice {
                target: SelectedTarget::App("test".to_owned()),
                label: "app  test".to_owned(),
                haystack: "app test run the suite".to_owned(),
            },
            CatalogChoice {
                target: SelectedTarget::Task("ci".to_owned()),
                label: "task ci".to_owned(),
                haystack: "task ci full gate".to_owned(),
            },
        ];
        let filtered = apply_substring_filter(&choices, Some("CI"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target, SelectedTarget::Task("ci".to_owned()));
    }

    #[test]
    fn format_app_label_includes_description_when_present() {
        let app = App {
            name: "test".to_owned(),
            attr_path: "apps.aarch64-darwin.test".to_owned(),
            flake_ref: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: Some("Run the test suite".to_owned()),
            is_default: false,
            metadata: BTreeMap::new(),
        };
        let label = format_entry_label("app", &app.name, app.description.as_deref());
        assert!(label.starts_with("app"));
        assert!(label.contains("test"));
        assert!(label.contains("Run the test suite"));
    }
}
