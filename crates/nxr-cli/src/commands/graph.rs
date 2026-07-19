//! `nxr graph` command implementation.

use std::io::{self, Write};

use clap::ValueEnum;
use nxr_core::diagnostics::exit;
use nxr_nix::TaskDiscoveryError;
use nxr_task::{
    GraphError as TaskGraphError, TaskGraph, plan_serial, render_mermaid, render_text,
    resolve_task_name,
};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::resolve_flake;
use crate::runner_output::RunnerOutput;

const SCHEMA_VERSION: u32 = 1;

/// Graph rendering format for human output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum GraphFormat {
    /// Newline-separated topological order.
    #[default]
    Text,
    /// Mermaid `flowchart TD` diagram.
    Mermaid,
}

impl GraphFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Mermaid => "mermaid",
        }
    }
}

/// Inputs for task graph rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub task: &'a str,
}

/// Errors while discovering or rendering a task graph.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Discover(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Plan(#[from] TaskGraphError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl GraphError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Discover(error) => error.exit_code(),
            Self::Plan(TaskGraphError::UnknownRoot { .. }) => exit::NOT_FOUND,
            Self::Plan(_) => exit::TASK_GRAPH,
            Self::Json(_) | Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Versioned graph envelope for `--json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct GraphEnvelope {
    schema_version: u32,
    task: String,
    format: &'static str,
    order: Vec<String>,
    edges: Vec<[String; 2]>,
}

/// Discover tasks, plan the subgraph for `request.task`, and print the graph.
///
/// # Errors
///
/// Returns [`GraphError`] when discovery, planning, or output fails.
pub fn run(
    request: &GraphRequest<'_>,
    format: GraphFormat,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), GraphError> {
    let doc = discover_task_document(request)?;
    let canonical = resolve_task_name(&doc, request.task)
        .map_err(|error| GraphError::Plan(TaskGraphError::UnknownRoot { root: error.name }))?;
    runner
        .info(format!("planning task graph for {canonical}"))
        .map_err(GraphError::Io)?;

    let order = plan_serial(&doc.tasks, canonical)?;
    let graph = TaskGraph::subgraph(&doc.tasks, canonical)?;

    let mut stdout = io::stdout().lock();
    if json {
        write_json_graph(&mut stdout, canonical, format, &order, &graph)?;
    } else {
        write_human_graph(&mut stdout, format, &order, &graph)?;
    }
    Ok(())
}

fn discover_task_document(
    request: &GraphRequest<'_>,
) -> Result<nxr_task::TaskDocument, GraphError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)
        .map_err(|error| GraphError::Prepare(PrepareError::Flake(error)))?;
    let adapter = build_adapter(request.nix_override)
        .map_err(|error| GraphError::Prepare(PrepareError::Nix(error)))?;
    adapter
        .discover_tasks(&flake.nix_ref)
        .map_err(GraphError::from)
}

fn write_json_graph(
    writer: &mut impl Write,
    task: &str,
    format: GraphFormat,
    order: &[String],
    graph: &TaskGraph,
) -> Result<(), GraphError> {
    let edges = graph
        .precedence_edges()
        .into_iter()
        .map(|(dependency, dependent)| [dependency, dependent])
        .collect();
    let envelope = GraphEnvelope {
        schema_version: SCHEMA_VERSION,
        task: task.to_owned(),
        format: format.as_str(),
        order: order.to_vec(),
        edges,
    };
    writeln!(writer, "{}", serde_json::to_string_pretty(&envelope)?)?;
    Ok(())
}

fn write_human_graph(
    writer: &mut impl Write,
    format: GraphFormat,
    order: &[String],
    graph: &TaskGraph,
) -> Result<(), GraphError> {
    let rendered = match format {
        GraphFormat::Text => render_text(order),
        GraphFormat::Mermaid => render_mermaid(graph),
    };
    write!(writer, "{rendered}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::diagnostics::exit;
    use nxr_task::{GraphError as TaskGraphError, TaskDefinition};

    use super::{GraphError, GraphFormat, GraphRequest, write_human_graph};
    use nxr_task::TaskGraph;

    fn task(deps: &[&str]) -> TaskDefinition {
        let mut def = TaskDefinition::new("app");
        def.depends_on = deps.iter().map(|dep| (*dep).to_owned()).collect();
        def
    }

    #[test]
    fn unknown_root_maps_to_not_found() {
        let error = GraphError::Plan(TaskGraphError::UnknownRoot {
            root: "missing".to_owned(),
        });
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn cycle_maps_to_task_graph() {
        let error = GraphError::Plan(TaskGraphError::Cycle {
            path: vec!["a".to_owned(), "b".to_owned(), "a".to_owned()],
        });
        assert_eq!(error.exit_code(), exit::TASK_GRAPH);
    }

    #[test]
    fn missing_dependency_maps_to_task_graph() {
        let error = GraphError::Plan(TaskGraphError::MissingDependency {
            task: "a".to_owned(),
            dependency: "ghost".to_owned(),
        });
        assert_eq!(error.exit_code(), exit::TASK_GRAPH);
    }

    #[test]
    fn human_text_render_matches_plan_text() {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), task(&[]));
        tasks.insert("lint".to_owned(), task(&[]));
        tasks.insert("test".to_owned(), task(&["fmt", "lint"]));

        let order = nxr_task::plan_serial(&tasks, "test").expect("plan");
        let graph = TaskGraph::subgraph(&tasks, "test").expect("graph");
        let mut output = Vec::new();
        write_human_graph(&mut output, GraphFormat::Text, &order, &graph).expect("write");
        assert_eq!(
            output,
            nxr_task::plan_text(&tasks, "test").unwrap().into_bytes()
        );
    }

    #[test]
    fn human_mermaid_render_matches_plan_mermaid() {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), task(&[]));
        tasks.insert("test".to_owned(), task(&["fmt"]));

        let order = nxr_task::plan_serial(&tasks, "test").expect("plan");
        let graph = TaskGraph::subgraph(&tasks, "test").expect("graph");
        let mut output = Vec::new();
        write_human_graph(&mut output, GraphFormat::Mermaid, &order, &graph).expect("write");
        assert_eq!(
            output,
            nxr_task::plan_mermaid(&tasks, "test").unwrap().into_bytes()
        );
    }

    #[test]
    fn graph_request_is_copyable() {
        let request = GraphRequest {
            flake_arg: Some("."),
            nix_override: None,
            task: "ci",
        };
        let copied = request;
        assert_eq!(request, copied);
    }
}
