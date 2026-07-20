//! Build the affected-analysis graph from discovered apps and tasks.

use std::collections::{BTreeMap, BTreeSet};

use nxr_core::App;
use nxr_task::{TaskDefinition, TaskDocument, WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION};
use serde_json::Value as JsonValue;

use crate::paths::normalize_relative_path;

/// Kind of operation node in the affected graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NodeKind {
    /// Leaf flake app (`apps.<system>.<name>`).
    App,
    /// Orchestration task (`nxr.<system>.tasks.<name>`).
    Task,
}

/// One node in the affected graph with declared path roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNode {
    pub kind: NodeKind,
    pub name: String,
    pub path_roots: Vec<String>,
    pub depends_on: Vec<String>,
    pub app: Option<String>,
}

/// Indexed graph for affected analysis.
#[derive(Clone, Debug, Default)]
pub struct AffectedGraph {
    pub nodes: BTreeMap<String, GraphNode>,
    pub dependents: BTreeMap<String, BTreeSet<String>>,
}

/// Build an affected graph from discovered apps and the task document.
#[must_use]
pub fn build_graph(apps: &[App], task_doc: &TaskDocument) -> AffectedGraph {
    let mut nodes = BTreeMap::new();
    let mut app_paths = BTreeMap::new();

    for app in apps {
        let roots = paths_from_metadata(&app.metadata);
        app_paths.insert(app.name.clone(), roots.clone());
        nodes.insert(
            node_key(NodeKind::App, &app.name),
            GraphNode {
                kind: NodeKind::App,
                name: app.name.clone(),
                path_roots: roots,
                depends_on: Vec::new(),
                app: None,
            },
        );
    }

    for (name, task) in &task_doc.tasks {
        let path_roots = task_path_roots(task, app_paths.get(&task.app));
        nodes.insert(
            node_key(NodeKind::Task, name),
            GraphNode {
                kind: NodeKind::Task,
                name: name.clone(),
                path_roots,
                depends_on: task.depends_on.clone(),
                app: Some(task.app.clone()),
            },
        );
    }

    let dependents = reverse_dependencies(&nodes);
    AffectedGraph { nodes, dependents }
}

fn node_key(kind: NodeKind, name: &str) -> String {
    match kind {
        NodeKind::App => format!("app:{name}"),
        NodeKind::Task => format!("task:{name}"),
    }
}

fn reverse_dependencies(nodes: &BTreeMap<String, GraphNode>) -> BTreeMap<String, BTreeSet<String>> {
    let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, node) in nodes {
        for dep in &node.depends_on {
            let dep_key = node_key(NodeKind::Task, dep);
            dependents
                .entry(dep_key)
                .or_default()
                .insert(key.clone());
        }
        if let Some(app_name) = &node.app {
            let app_key = node_key(NodeKind::App, app_name);
            dependents
                .entry(app_key)
                .or_default()
                .insert(key.clone());
        }
    }
    dependents
}

fn task_path_roots(task: &TaskDefinition, app_roots: Option<&Vec<String>>) -> Vec<String> {
    let mut roots = task.paths.clone();
    if let Some(working_directory) = working_directory_root(task.working_directory.as_deref()) {
        roots.push(working_directory);
    }
    if let Some(app_roots) = app_roots {
        roots.extend(app_roots.iter().cloned());
    }
    dedupe_paths(roots)
}

fn working_directory_root(working_directory: Option<&str>) -> Option<String> {
    let value = working_directory?;
    if value == WORKING_DIRECTORY_INVOCATION || value == WORKING_DIRECTORY_FLAKE_ROOT {
        return None;
    }
    Some(normalize_relative_path(value))
}

fn paths_from_metadata(metadata: &BTreeMap<String, JsonValue>) -> Vec<String> {
    metadata
        .get("paths")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(|path| normalize_relative_path(path))
                .collect()
        })
        .unwrap_or_default()
}

fn dedupe_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let normalized = normalize_relative_path(&path);
        if seen.insert(normalized.clone()) {
            deduped.push(normalized);
        }
    }
    deduped
}
