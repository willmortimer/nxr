//! `nxr inspect` command implementation.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::diagnostics::exit;
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_core::{App, AppList, ListApp, ProjectsError};
use nxr_nix::{
    AppNotFoundError, NixError, OptionalNixFlags, TaskDiscoveryError, resolve_app_by_name,
};
use nxr_task::{TaskDefinition, TaskDocument, resolve_task};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::commands::views::ViewFilter;
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// What to inspect: flake overview, one app, or one task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectTarget {
    /// Schema version plus discovered apps and tasks.
    Overview,
    /// A single app by name.
    App { name: String },
    /// A single task by name.
    Task { name: String },
}

/// Inputs for `nxr inspect`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub target: InspectTarget,
    /// When set, overview listings include only apps/tasks in this category.
    pub category: Option<&'a str>,
    /// When set, overview listings include only members of this project namespace.
    pub namespace: Option<&'a str>,
}

/// Errors while running the inspect command.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Projects(#[from] ProjectsError),
    #[error(transparent)]
    AppNotFound(#[from] AppNotFoundError),
    #[error(transparent)]
    TaskNotFound(#[from] TaskNotFoundError),
    #[error(transparent)]
    Render(#[from] InspectRenderError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl InspectError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Projects(error) => error.exit_code(),
            Self::AppNotFound(error) => error.exit_code(),
            Self::TaskNotFound(_) => TaskNotFoundError::exit_code(),
            Self::Render(_) | Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Errors while rendering inspect output.
#[derive(Debug, thiserror::Error)]
pub enum InspectRenderError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// No discovered task matches the requested name.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TaskNotFoundError {
    pub name: String,
    pub suggestions: Vec<String>,
}

impl fmt::Display for TaskNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task not found: {}", self.name)?;
        if self.suggestions.is_empty() {
            return Ok(());
        }

        writeln!(f)?;
        writeln!(f)?;
        writeln!(f, "Did you mean:")?;
        for suggestion in &self.suggestions {
            writeln!(f, "  {suggestion}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TaskNotFoundError {}

impl TaskNotFoundError {
    #[must_use]
    pub const fn exit_code() -> i32 {
        exit::NOT_FOUND
    }
}

const OVERVIEW_SCHEMA_VERSION: u32 = 1;
const DETAIL_SCHEMA_VERSION: u32 = 1;

/// Discover flake metadata and print inspect output.
///
/// # Errors
///
/// Returns [`InspectError`] when discovery or rendering fails.
pub fn run(
    request: InspectRequest<'_>,
    json: bool,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), InspectError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    runner
        .info(format!("inspecting {}", flake.display))
        .map_err(InspectError::Io)?;
    let adapter = build_adapter(request.nix_override)?;

    match request.target {
        InspectTarget::Overview => {
            let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
            let task_doc = workspace.tasks.expect("overview discovers tasks with apps");
            let filter = ViewFilter::resolve(
                flake
                    .local_root
                    .as_deref()
                    .map(camino::Utf8Path::as_std_path),
                request.category,
                request.namespace,
            )?;
            let apps = filter.filter_apps(&workspace.apps, &task_doc);
            let mut stdout = io::stdout().lock();
            write_overview(
                &mut stdout,
                &flake,
                &adapter.system,
                &apps,
                &task_doc,
                &filter,
                json,
            )?;
        }
        InspectTarget::App { name } => {
            let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
            let task_doc = workspace
                .tasks
                .expect("app inspect discovers tasks with apps");
            let mut apps = workspace.apps;
            nxr_task::enrich_apps_with_listing_metadata(&mut apps, &task_doc);
            let app = resolve_app_by_name(&apps, &name)?;
            let mut stdout = io::stdout().lock();
            write_app(&mut stdout, &flake, &adapter.system, app, json)?;
        }
        InspectTarget::Task { name } => {
            let task_doc = discover_tasks(&flake, &adapter, refresh_discovery, nix_flags)?;
            let (canonical, task) = resolve_task(&task_doc, &name).map_err(map_resolve_error)?;
            let mut stdout = io::stdout().lock();
            write_task(&mut stdout, &flake, &adapter.system, canonical, task, json)?;
        }
    }

    Ok(())
}

fn discover_workspace(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<WorkspaceDiscovery, InspectError> {
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
                .map_err(InspectError::Nix)?;
            let tasks = adapter
                .discover_tasks(&flake_ref, nix_flags)
                .map_err(InspectError::Tasks)?;
            Ok(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
            })
        },
    )
}

fn discover_tasks(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, InspectError> {
    let workspace = discover_workspace(flake, adapter, refresh_discovery, nix_flags)?;
    Ok(workspace
        .tasks
        .expect("task inspect discovers tasks with apps"))
}

fn map_resolve_error(error: nxr_task::ResolveTaskError) -> TaskNotFoundError {
    TaskNotFoundError {
        name: error.name,
        suggestions: error.suggestions,
    }
}

#[derive(Serialize)]
struct InspectOverviewJson<'a> {
    schema_version: u32,
    flake: &'a str,
    system: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_schema_version: Option<u32>,
    apps: Vec<ListApp>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tasks: BTreeMap<String, TaskDefinition>,
}

#[derive(Serialize)]
struct InspectAppJson<'a> {
    schema_version: u32,
    flake: &'a str,
    system: &'a str,
    kind: &'static str,
    app: &'a App,
}

#[derive(Serialize)]
struct InspectTaskJson<'a> {
    schema_version: u32,
    flake: &'a str,
    system: &'a str,
    kind: &'static str,
    name: &'a str,
    #[serde(flatten)]
    task: &'a TaskDefinition,
}

fn write_overview(
    writer: &mut impl Write,
    flake: &FlakeSelection,
    system: &str,
    apps: &[App],
    task_doc: &TaskDocument,
    filter: &ViewFilter,
    json: bool,
) -> Result<(), InspectRenderError> {
    let tasks = filter.filter_tasks(task_doc);
    if json {
        let envelope = InspectOverviewJson {
            schema_version: OVERVIEW_SCHEMA_VERSION,
            flake: &flake.display,
            system,
            task_schema_version: (!tasks.is_empty()).then_some(task_doc.schema_version),
            apps: AppList::from_apps(&flake.display, system, apps.iter().cloned()).apps,
            tasks,
        };
        let rendered = serde_json::to_string_pretty(&envelope)?;
        writeln!(writer, "{rendered}")?;
        return Ok(());
    }

    writeln!(writer, "Flake: {}", flake.display)?;
    writeln!(writer, "System: {system}")?;
    writeln!(writer)?;
    writeln!(writer, "Apps:")?;
    if apps.is_empty() {
        writeln!(writer, "  (none)")?;
    } else {
        write_human_apps(writer, apps)?;
    }

    if !tasks.is_empty() {
        writeln!(writer)?;
        writeln!(
            writer,
            "Tasks (schema version {}):",
            task_doc.schema_version
        )?;
        write_human_tasks(writer, &tasks)?;
    }

    Ok(())
}

fn write_app(
    writer: &mut impl Write,
    flake: &FlakeSelection,
    system: &str,
    app: &App,
    json: bool,
) -> Result<(), InspectRenderError> {
    if json {
        let envelope = InspectAppJson {
            schema_version: DETAIL_SCHEMA_VERSION,
            flake: &flake.display,
            system,
            kind: "app",
            app,
        };
        let rendered = serde_json::to_string_pretty(&envelope)?;
        writeln!(writer, "{rendered}")?;
        return Ok(());
    }

    writeln!(writer, "App: {}", app.name)?;
    writeln!(writer, "Attr: {}", app.attr_path)?;
    if let Some(description) = &app.description {
        writeln!(
            writer,
            "Description: {}",
            sanitize_terminal_text(description)
        )?;
    }
    writeln!(writer, "Default: {}", app.is_default)?;
    if let Some(category) = nxr_core::app_category(app) {
        writeln!(writer, "Category: {}", sanitize_terminal_text(category))?;
    }
    if !app.metadata.is_empty() {
        writeln!(writer, "Metadata: {} key(s)", app.metadata.len())?;
    }
    Ok(())
}

fn write_task(
    writer: &mut impl Write,
    flake: &FlakeSelection,
    system: &str,
    name: &str,
    task: &TaskDefinition,
    json: bool,
) -> Result<(), InspectRenderError> {
    if json {
        let envelope = InspectTaskJson {
            schema_version: DETAIL_SCHEMA_VERSION,
            flake: &flake.display,
            system,
            kind: "task",
            name,
            task,
        };
        let rendered = serde_json::to_string_pretty(&envelope)?;
        writeln!(writer, "{rendered}")?;
        return Ok(());
    }

    writeln!(writer, "Task: {name}")?;
    writeln!(writer, "App: {}", task.app)?;
    if let Some(description) = &task.description {
        writeln!(
            writer,
            "Description: {}",
            sanitize_terminal_text(description)
        )?;
    }
    if !task.depends_on.is_empty() {
        writeln!(writer, "Depends on: {}", task.depends_on.join(", "))?;
    }
    if let Some(working_directory) = &task.working_directory {
        writeln!(writer, "Working directory: {working_directory}")?;
    }
    if task.hidden {
        writeln!(writer, "Hidden: true")?;
    }
    if let Some(category) = &task.category {
        writeln!(writer, "Category: {}", sanitize_terminal_text(category))?;
    }
    Ok(())
}

fn write_human_apps(writer: &mut impl Write, apps: &[App]) -> io::Result<()> {
    let max_name_len = apps
        .iter()
        .map(|app| app.name.len())
        .max()
        .unwrap_or_default();
    let name_width = max_name_len.max(4).max(max_name_len.saturating_add(1));

    for app in apps {
        let name = app.name.as_str();
        write!(writer, "  {name:<name_width$}")?;
        if let Some(description) = &app.description {
            writeln!(writer, "{}", sanitize_terminal_text(description))?;
        } else {
            writeln!(writer)?;
        }
    }
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

    use nxr_core::App;
    use nxr_task::TaskDefinition;

    use nxr_task::{TaskDocument, listable_tasks, resolve_task};

    use super::write_overview;
    use crate::commands::views::ViewFilter;
    use crate::flake::FlakeSelection;

    fn sample_apps() -> Vec<App> {
        vec![
            App {
                name: "hello".to_owned(),
                attr_path: "apps.aarch64-darwin.hello".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Say hello".to_owned()),
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
        ]
    }

    fn sample_task_doc() -> TaskDocument {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), TaskDefinition::new("fmt"));
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: Some("CI gate".to_owned()),
                depends_on: vec!["test".to_owned()],
                app: "ci".to_owned(),
                working_directory: None,
                hidden: false,
                category: Some("validation".to_owned()),
                aliases: Vec::new(),
                interactive: false,
            },
        );
        tasks.insert(
            "hidden-task".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "hello".to_owned(),
                working_directory: None,
                hidden: true,
                category: None,
                aliases: Vec::new(),
                interactive: false,
            },
        );
        TaskDocument::new(tasks)
    }

    #[test]
    fn listable_tasks_omit_hidden() {
        let doc = sample_task_doc();
        let visible = listable_tasks(&doc, None);
        assert_eq!(visible.len(), 2);
        assert!(visible.contains_key("fmt"));
        assert!(visible.contains_key("ci"));
        assert!(!visible.contains_key("hidden-task"));
    }

    #[test]
    fn resolve_task_finds_hidden_task() {
        let doc = sample_task_doc();
        let (_, task) = resolve_task(&doc, "hidden-task").expect("hidden task");
        assert!(task.hidden);
    }

    #[test]
    fn resolve_task_suggests_prefix_match() {
        let doc = sample_task_doc();
        let error = nxr_task::resolve_task_name(&doc, "c").expect_err("ambiguous prefix");
        assert_eq!(error.name, "c");
        assert!(error.suggestions.contains(&"ci".to_owned()));
    }

    #[test]
    fn listable_tasks_filter_by_category() {
        let doc = sample_task_doc();
        let validation = listable_tasks(&doc, Some("validation"));
        assert_eq!(validation.len(), 1);
        assert!(validation.contains_key("ci"));
    }

    #[test]
    fn overview_json_includes_tasks_when_present() {
        let flake = FlakeSelection {
            display: "fixtures/task-dag".to_owned(),
            nix_ref: "/abs/task-dag".to_owned(),
            local_root: None,
        };
        let mut output = Vec::new();
        write_overview(
            &mut output,
            &flake,
            "aarch64-darwin",
            &sample_apps(),
            &sample_task_doc(),
            &ViewFilter::default(),
            true,
        )
        .expect("write overview json");

        let value: serde_json::Value = serde_json::from_slice(&output).expect("parse json");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["task_schema_version"], 1);
        assert_eq!(value["apps"].as_array().expect("apps").len(), 2);
        assert_eq!(value["tasks"].as_object().expect("tasks").len(), 2);
        assert!(value["tasks"]["ci"]["dependsOn"].is_array());
    }

    #[test]
    fn human_overview_lists_apps_and_tasks() {
        let flake = FlakeSelection {
            display: ".".to_owned(),
            nix_ref: ".".to_owned(),
            local_root: None,
        };
        let mut output = Vec::new();
        write_overview(
            &mut output,
            &flake,
            "aarch64-darwin",
            &sample_apps(),
            &sample_task_doc(),
            &ViewFilter::default(),
            false,
        )
        .expect("write overview");

        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("Apps:"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("Tasks (schema version 1):"));
        assert!(rendered.contains("ci"));
        assert!(!rendered.contains("hidden-task"));
    }
}
