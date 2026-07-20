//! `nxr list` command implementation.

use std::collections::BTreeMap;
use std::io::{self, Write};

use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_core::{App, AppList};
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_task::{TaskDefinition, TaskDocument, listable_tasks};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::output::{JsonListError, write_human_list, write_json_list};
use crate::runner_output::RunnerOutput;

/// Errors while running the list command.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Json(#[from] JsonListError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ListError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Json(_) | Self::Io(_) => nxr_core::diagnostics::exit::EVALUATION,
        }
    }
}

/// Discover and print apps (and tasks when present) for the selected flake.
///
/// # Errors
///
/// Returns [`ListError`] when flake resolution, Nix discovery, or output fails.
pub fn run(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    category: Option<&str>,
    runner: RunnerOutput,
) -> Result<(), ListError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    runner
        .info(format!("discovering apps for {}", flake.display))
        .map_err(ListError::Io)?;
    let adapter = build_adapter(nix_override)?;
    let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
    let apps = workspace.apps;
    let task_doc = workspace
        .tasks
        .expect("list always discovers tasks with apps");
    let tasks = listable_tasks(&task_doc, category);
    runner
        .verbose(format!(
            "found {} app(s) and {} task(s) for system {}",
            apps.len(),
            tasks.len(),
            adapter.system
        ))
        .map_err(ListError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        if tasks.is_empty() {
            write_json_list(&mut stdout, &flake.display, &adapter.system, &apps)?;
        } else {
            write_json_list_with_tasks(
                &mut stdout,
                &flake.display,
                &adapter.system,
                &apps,
                &task_doc,
                &tasks,
            )?;
        }
    } else {
        write_human_list(&mut stdout, &adapter.system, &apps)?;
        if !tasks.is_empty() {
            writeln!(stdout)?;
            writeln!(
                stdout,
                "Available tasks (schema version {}):",
                task_doc.schema_version
            )?;
            writeln!(stdout)?;
            write_human_tasks(&mut stdout, &tasks)?;
        }
    }

    Ok(())
}

fn discover_workspace(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<WorkspaceDiscovery, ListError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
    };
    let flake_ref = flake.nix_ref.clone();

    discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions::with_tasks(refresh_discovery),
        || {
            let apps = adapter
                .discover_apps(&flake_ref, nix_flags)
                .map_err(ListError::Nix)?;
            let tasks = adapter
                .discover_tasks(&flake_ref, nix_flags)
                .map_err(ListError::Tasks)?;
            Ok(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
            })
        },
    )
}

#[derive(Serialize)]
struct ListEnvelope {
    #[serde(flatten)]
    apps: AppList,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_schema_version: Option<u32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tasks: BTreeMap<String, TaskDefinition>,
}

fn write_json_list_with_tasks(
    writer: &mut impl Write,
    flake: &str,
    system: &str,
    apps: &[App],
    task_doc: &TaskDocument,
    tasks: &BTreeMap<String, TaskDefinition>,
) -> Result<(), JsonListError> {
    let envelope = ListEnvelope {
        apps: AppList::from_apps(flake, system, apps.iter().cloned()),
        task_schema_version: (!tasks.is_empty()).then_some(task_doc.schema_version),
        tasks: tasks.clone(),
    };
    let json = serde_json::to_string_pretty(&envelope)?;
    writeln!(writer, "{json}")?;
    Ok(())
}

fn write_human_tasks(
    writer: &mut impl Write,
    tasks: &BTreeMap<String, TaskDefinition>,
) -> io::Result<()> {
    let max_name_len = tasks.keys().map(String::len).max().unwrap_or_default();
    let name_width = max_name_len.max(4).max(max_name_len.saturating_add(1));

    for (name, task) in tasks {
        write!(writer, "  {name:<name_width$}")?;
        if let Some(description) = &task.description {
            writeln!(writer, "{}", sanitize_terminal_text(description))?;
        } else {
            writeln!(writer)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_task::{TaskDefinition, TaskDocument, listable_tasks};

    use crate::commands::common::current_invocation_directory;

    #[test]
    fn invocation_directory_is_valid_utf8_path() {
        let cwd = current_invocation_directory().expect("current directory");
        assert!(cwd.is_absolute() || !cwd.as_str().is_empty());
    }

    #[test]
    fn listable_tasks_honor_hidden_and_category() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "ci".to_owned(),
                working_directory: None,
                hidden: false,
                category: Some("validation".to_owned()),
                aliases: Vec::new(),
            },
        );
        tasks.insert(
            "hidden".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "x".to_owned(),
                working_directory: None,
                hidden: true,
                category: Some("validation".to_owned()),
                aliases: Vec::new(),
            },
        );
        let doc = TaskDocument::new(tasks);
        let filtered = listable_tasks(&doc, Some("validation"));
        assert_eq!(filtered.len(), 1);
        assert!(filtered.contains_key("ci"));
    }
}
