//! Optional `.nxr/workflows.toml` operation graph.
//!
//! The file is not required. Repositories that lack it are unchanged, and bare
//! `nxr <name>` resolution does not consult it. A workflow graph is planned
//! only when the file is present ([`WorkflowPlanMode::IfPresent`]) or an
//! explicit `workflow` / `workflow:<task>` selector asks for it.
//!
//! The file names tasks, dependencies, a platform, and one executor reference.
//! It does not embed build scripts.
//!
//! ```toml
//! schema_version = 1
//!
//! [tasks.prep]
//! platform = "unix" # any | unix | windows; default any
//! depends_on = [] # alias: dependsOn
//! executor = { native = ["/bin/echo", "prep"] }
//!
//! [tasks.fmt]
//! depends_on = ["prep"]
//! executor = { flake = "fmt" }
//!
//! [tasks.win]
//! platform = "windows"
//! executor = { winbuild = "link-release" }
//! ```
//!
//! `executor` sets exactly one of `flake` (app name), `native` (argv), or
//! `winbuild` (task id).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nxr_core::Operation;
use serde::Deserialize;
use thiserror::Error;

use crate::events::{Event, EventSink};
use crate::graph::{GraphError, TaskGraph};
use crate::plan_exec::{ArgumentForwarding, ExecutionPlan, FailurePolicy, PlanNode};
use crate::planner::plan_serial_union;
use crate::resources::NodeResources;
use crate::schema::TaskDefinition;

/// Supported major version for `.nxr/workflows.toml`.
pub const WORKFLOWS_SCHEMA_VERSION: u32 = 1;

/// Repo-relative path of the optional workflow file.
pub const WORKFLOWS_RELATIVE_PATH: &str = ".nxr/workflows.toml";

/// How a caller asked to plan a workflow graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowPlanMode {
    /// Plan when the file exists. A missing file is not an error.
    IfPresent,
    /// `workflow` (entire graph) or `workflow:<task>`.
    ///
    /// A missing file is an error. This is never inferred from a bare name.
    Explicit { root: Option<String> },
}

/// Platform named by a workflow task.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkflowPlatform {
    /// No platform constraint.
    #[default]
    Any,
    /// Unix-like host.
    Unix,
    /// Windows host.
    Windows,
}

impl WorkflowPlatform {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "any" => Some(Self::Any),
            "unix" => Some(Self::Unix),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }

    /// Label stored on a plan node. `any` is omitted.
    #[must_use]
    pub fn as_plan_label(self) -> Option<String> {
        match self {
            Self::Any => None,
            Self::Unix => Some("unix".to_owned()),
            Self::Windows => Some("windows".to_owned()),
        }
    }
}

/// One named workflow task after executor desugaring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowTask {
    pub depends_on: Vec<String>,
    pub platform: WorkflowPlatform,
    pub operation: Operation,
}

/// Parsed `.nxr/workflows.toml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDocument {
    pub schema_version: u32,
    pub tasks: BTreeMap<String, WorkflowTask>,
}

/// Errors while loading or planning an optional workflow file.
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("workflow file {path} was not valid TOML: {message}")]
    Parse { path: String, message: String },
    #[error(
        "unsupported workflows schema version {found} in {path}; expected major version {expected}"
    )]
    UnsupportedVersion {
        path: String,
        found: u32,
        expected: u32,
    },
    #[error("invalid workflows file {path}: {message}")]
    Invalid { path: String, message: String },
    #[error("workflow selector is missing a task name")]
    EmptySelector,
    #[error("optional workflow file {path} is not present")]
    MissingFile { path: String },
    #[error(transparent)]
    Plan(#[from] GraphError),
}

impl WorkflowError {
    /// Existing public exit code. This does not add a new process status.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;
        match self {
            Self::Io { .. }
            | Self::Parse { .. }
            | Self::UnsupportedVersion { .. }
            | Self::Invalid { .. } => exit::INVALID_METADATA,
            Self::EmptySelector => exit::USAGE,
            Self::MissingFile { .. } => exit::NOT_FOUND,
            Self::Plan(error) => match error {
                GraphError::UnknownRoot { .. } => exit::NOT_FOUND,
                GraphError::MissingDependency { .. } | GraphError::Cycle { .. } => exit::TASK_GRAPH,
            },
        }
    }
}

/// Map `workflow` and `workflow:<task>` selectors.
///
/// Any other token, including a bare app or task name, returns [`None`].
/// Callers must leave flake resolution alone in that case.
#[must_use]
pub fn workflow_mode_from_selector(token: &str) -> Option<WorkflowPlanMode> {
    if token == "workflow" {
        return Some(WorkflowPlanMode::Explicit { root: None });
    }
    token
        .strip_prefix("workflow:")
        .map(|name| WorkflowPlanMode::Explicit {
            root: Some(name.to_owned()),
        })
}

/// Path of the optional workflow file under `repo_root`.
#[must_use]
pub fn workflow_path(repo_root: &Path) -> PathBuf {
    repo_root.join(WORKFLOWS_RELATIVE_PATH)
}

/// Load `.nxr/workflows.toml` when it exists.
///
/// A missing file returns `Ok(None)` and does not invent tasks.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the file exists but cannot be read or validated.
pub fn load_workflow_document(repo_root: &Path) -> Result<Option<WorkflowDocument>, WorkflowError> {
    let path = workflow_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|source| WorkflowError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_workflow_str(&text, &path.display().to_string()).map(Some)
}

/// Plan a workflow graph for `mode`.
///
/// [`WorkflowPlanMode::IfPresent`] returns `Ok(None)` when the file is absent.
/// [`WorkflowPlanMode::Explicit`] returns [`WorkflowError::MissingFile`] instead.
///
/// # Errors
///
/// Returns [`WorkflowError`] for a missing explicit file, an invalid document,
/// or a task-graph planning failure.
pub fn plan_workflows(
    repo_root: &Path,
    mode: WorkflowPlanMode,
    failure_policy: FailurePolicy,
    sink: Option<&mut dyn EventSink>,
) -> Result<Option<ExecutionPlan>, WorkflowError> {
    let Some(document) = load_workflow_document(repo_root)? else {
        return match mode {
            WorkflowPlanMode::IfPresent => Ok(None),
            WorkflowPlanMode::Explicit { .. } => Err(WorkflowError::MissingFile {
                path: workflow_path(repo_root).display().to_string(),
            }),
        };
    };
    plan_workflow_document(&document, &mode, failure_policy, sink).map(Some)
}

/// Plan `document` without touching the filesystem.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the selector or graph is invalid.
pub fn plan_workflow_document(
    document: &WorkflowDocument,
    mode: &WorkflowPlanMode,
    failure_policy: FailurePolicy,
    sink: Option<&mut dyn EventSink>,
) -> Result<ExecutionPlan, WorkflowError> {
    if document.tasks.is_empty() {
        return Err(WorkflowError::Invalid {
            path: WORKFLOWS_RELATIVE_PATH.to_owned(),
            message: "workflows document has no tasks".to_owned(),
        });
    }

    let all_names: Vec<String> = document.tasks.keys().cloned().collect();
    let single_root: Option<&str> = match mode {
        WorkflowPlanMode::Explicit { root: Some(name) } if name.is_empty() => {
            return Err(WorkflowError::EmptySelector);
        }
        WorkflowPlanMode::Explicit { root: Some(name) } => Some(name.as_str()),
        WorkflowPlanMode::IfPresent | WorkflowPlanMode::Explicit { root: None } => None,
    };
    let all_refs: Vec<&str> = all_names.iter().map(String::as_str).collect();
    let roots: Vec<&str> = match single_root {
        Some(name) => vec![name],
        None => all_refs,
    };

    let synthetic = synthetic_tasks(document);
    let serial_order = plan_serial_union(&synthetic, &roots)?;
    let graph = TaskGraph::subgraph_union(&synthetic, &roots)?;

    let nodes: Vec<PlanNode> = graph
        .node_ids()
        .filter_map(|id| {
            let task = document.tasks.get(id)?;
            Some(PlanNode {
                id: id.to_owned(),
                depends_on: graph
                    .dependencies(id)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default(),
                interactive: false,
                resources: NodeResources::default(),
                operation: Some(task.operation.clone()),
                platform: task.platform.as_plan_label(),
            })
        })
        .collect();

    let primary_root = roots
        .first()
        .map(|root| (*root).to_owned())
        .expect("empty documents are rejected");
    let extra_roots = if roots.len() > 1 {
        roots.iter().map(|root| (*root).to_owned()).collect()
    } else {
        Vec::new()
    };
    let waves = serial_order.iter().cloned().map(|id| vec![id]).collect();

    let plan = ExecutionPlan {
        schema_version: ExecutionPlan::SCHEMA_VERSION,
        root: primary_root,
        roots: extra_roots,
        failure_policy,
        argument_forwarding: ArgumentForwarding::Root,
        nodes,
        serial_order,
        waves,
    };

    if let Some(sink) = sink {
        sink.emit(Event::plan_created(
            plan.root.clone(),
            if plan.roots.is_empty() {
                None
            } else {
                Some(plan.roots.clone())
            },
            plan.nodes.len(),
        ));
    }

    Ok(plan)
}

/// Graph-only stand-in so workflow edges reuse the task planner.
///
/// The placeholder `app` is discarded. Plan nodes carry [`WorkflowTask::operation`].
fn synthetic_tasks(document: &WorkflowDocument) -> BTreeMap<String, TaskDefinition> {
    document
        .tasks
        .iter()
        .map(|(name, task)| {
            let mut definition = TaskDefinition::new("workflow");
            definition.depends_on.clone_from(&task.depends_on);
            (name.clone(), definition)
        })
        .collect()
}

fn parse_workflow_str(text: &str, path: &str) -> Result<WorkflowDocument, WorkflowError> {
    let file: WorkflowFile = toml::from_str(text).map_err(|error| WorkflowError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    if file.schema_version != WORKFLOWS_SCHEMA_VERSION {
        return Err(WorkflowError::UnsupportedVersion {
            path: path.to_owned(),
            found: file.schema_version,
            expected: WORKFLOWS_SCHEMA_VERSION,
        });
    }

    let mut tasks = BTreeMap::new();
    for (name, task) in file.tasks {
        validate_task_name(&name).map_err(|message| WorkflowError::Invalid {
            path: path.to_owned(),
            message,
        })?;
        let platform = match task.platform.as_deref() {
            None => WorkflowPlatform::Any,
            Some(raw) => WorkflowPlatform::parse(raw).ok_or_else(|| WorkflowError::Invalid {
                path: path.to_owned(),
                message: format!("task `{name}` platform `{raw}` is not any, unix, or windows"),
            })?,
        };
        let operation = operation_from_executor(&name, task.executor).map_err(|message| {
            WorkflowError::Invalid {
                path: path.to_owned(),
                message,
            }
        })?;
        tasks.insert(
            name,
            WorkflowTask {
                depends_on: task.depends_on,
                platform,
                operation,
            },
        );
    }

    Ok(WorkflowDocument {
        schema_version: file.schema_version,
        tasks,
    })
}

fn validate_task_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.chars().any(char::is_whitespace)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(format!("invalid workflow task name `{name}`"));
    }
    Ok(())
}

fn operation_from_executor(
    task: &str,
    executor: WorkflowExecutorFile,
) -> Result<Operation, String> {
    let selected = [
        executor.flake.is_some(),
        executor.native.is_some(),
        executor.winbuild.is_some(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();
    if selected != 1 {
        return Err(format!(
            "task `{task}` executor must set exactly one of flake, native, or winbuild"
        ));
    }
    if let Some(app) = executor.flake {
        validate_token(&format!("task `{task}` flake app"), &app)?;
        return Ok(Operation::FlakeApp { name: app });
    }
    if let Some(argv) = executor.native {
        if argv.is_empty() || argv.iter().any(String::is_empty) {
            return Err(format!(
                "task `{task}` native argv must be a non-empty argument list"
            ));
        }
        return Ok(Operation::NativeCommand { argv });
    }
    if let Some(task_id) = executor.winbuild {
        validate_token(&format!("task `{task}` winbuild task id"), &task_id)?;
        return Ok(Operation::WinBuildTask { task_id });
    }
    Err(format!("task `{task}` executor is missing"))
}

fn validate_token(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{label} must be a single token"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowFile {
    schema_version: u32,
    #[serde(default)]
    tasks: BTreeMap<String, WorkflowTaskFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowTaskFile {
    #[serde(default, alias = "dependsOn")]
    depends_on: Vec<String>,
    #[serde(default)]
    platform: Option<String>,
    executor: WorkflowExecutorFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowExecutorFile {
    #[serde(default)]
    flake: Option<String>,
    #[serde(default)]
    native: Option<Vec<String>>,
    #[serde(default)]
    winbuild: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nxr_core::Operation;
    use nxr_core::diagnostics::exit;

    use super::*;
    use crate::events::{RecordingSink, event_kind};
    use crate::execute::{ExecContext, run_operation_plan};

    fn parse(text: &str) -> WorkflowDocument {
        parse_workflow_str(text, "workflows.toml").expect("parse workflows")
    }

    #[test]
    fn bare_names_are_not_workflow_selectors() {
        for token in ["ci", "fmt", "test", "build", "app:hello", "task:ci"] {
            assert_eq!(workflow_mode_from_selector(token), None, "{token}");
        }
        assert_eq!(
            workflow_mode_from_selector("workflow"),
            Some(WorkflowPlanMode::Explicit { root: None })
        );
        assert_eq!(
            workflow_mode_from_selector("workflow:finish"),
            Some(WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned())
            })
        );
    }

    #[test]
    fn missing_file_does_not_invent_a_graph() {
        let root = tempfile::tempdir().expect("tempdir");
        let loaded = load_workflow_document(root.path()).expect("load");
        assert!(loaded.is_none());
        let planned = plan_workflows(
            root.path(),
            WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect("if present");
        assert!(planned.is_none());

        let explicit = plan_workflows(
            root.path(),
            WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("explicit missing file");
        assert!(matches!(explicit, WorkflowError::MissingFile { .. }));
        assert_eq!(explicit.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn rejects_inline_script_bodies() {
        let text = r#"
schema_version = 1

[tasks.prep]
script = "echo copied-build-script"
executor = { native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(text, "workflows.toml").expect_err("script field");
        assert!(matches!(error, WorkflowError::Parse { .. }));
        assert_eq!(error.exit_code(), exit::INVALID_METADATA);
    }

    #[test]
    fn rejects_two_executors_and_unknown_platform() {
        let both = r#"
schema_version = 1

[tasks.prep]
executor = { flake = "fmt", native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(both, "workflows.toml").expect_err("two executors");
        assert!(matches!(error, WorkflowError::Invalid { .. }));

        let platform = r#"
schema_version = 1

[tasks.prep]
platform = "macos"
executor = { native = ["/bin/echo", "prep"] }
"#;
        let error = parse_workflow_str(platform, "workflows.toml").expect_err("platform");
        assert!(matches!(error, WorkflowError::Invalid { .. }));
    }

    #[test]
    fn winbuild_reference_plans_on_any_host() {
        let document = parse(
            r#"
schema_version = 1

[tasks.win]
platform = "windows"
dependsOn = []
executor = { winbuild = "link-release" }
"#,
        );
        let plan = plan_workflow_document(
            &document,
            &WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect("plan");
        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].platform.as_deref(), Some("windows"));
        assert_eq!(
            plan.nodes[0].operation,
            Some(Operation::WinBuildTask {
                task_id: "link-release".to_owned()
            })
        );
    }

    #[test]
    fn cycle_maps_to_task_graph_exit() {
        let document = parse(
            r#"
schema_version = 1

[tasks.a]
depends_on = ["b"]
executor = { native = ["/bin/true"] }

[tasks.b]
depends_on = ["a"]
executor = { native = ["/bin/true"] }
"#,
        );
        let error = plan_workflow_document(
            &document,
            &WorkflowPlanMode::IfPresent,
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("cycle");
        assert!(matches!(
            error,
            WorkflowError::Plan(GraphError::Cycle { .. })
        ));
        assert_eq!(error.exit_code(), exit::TASK_GRAPH);
    }

    #[test]
    fn native_workflow_fixture_plans_and_runs_without_nix() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/native-workflow");
        let mut sink = RecordingSink::new();
        let plan = plan_workflows(
            &fixture,
            WorkflowPlanMode::Explicit {
                root: Some("finish".to_owned()),
            },
            FailurePolicy::FailFast,
            Some(&mut sink),
        )
        .expect("plan fixture")
        .expect("file present");

        assert_eq!(plan.root, "finish");
        assert!(plan.roots.is_empty());
        assert_eq!(
            plan.serial_order,
            vec!["prep".to_owned(), "mid".to_owned(), "finish".to_owned()]
        );
        assert_eq!(
            sink.events(),
            &[Event::plan_created("finish".to_owned(), None, 3)]
        );
        assert_eq!(event_kind(&sink.events()[0]), "plan_created");

        let ids: Vec<_> = plan.node_ids().collect();
        assert_eq!(ids, vec!["finish", "mid", "prep"]);
        assert!(plan.nodes.iter().all(|node| {
            matches!(node.operation, Some(Operation::NativeCommand { .. }))
                && node.platform.as_deref() == Some("unix")
        }));

        #[cfg(unix)]
        {
            let trap_dir = tempfile::tempdir().expect("trap dir");
            let nix_marker = trap_dir.path().join("nix-ran");
            let wsl_marker = trap_dir.path().join("wsl-ran");
            write_trap(trap_dir.path(), "nix", &nix_marker);
            write_trap(trap_dir.path(), "wsl", &wsl_marker);
            let ctx = ExecContext {
                path: trap_dir.path().display().to_string(),
                nix_override: Some("/definitely/missing/nix".to_owned()),
                cwd: None,
            };
            let mut run_sink = RecordingSink::new();
            let run = run_operation_plan(&plan, &ctx, 1, Some(&mut run_sink)).expect("run native");
            assert_eq!(run.exit_code, 0);
            assert_eq!(
                run.order,
                vec!["prep".to_owned(), "mid".to_owned(), "finish".to_owned()]
            );
            assert_eq!(run.stdout, "prep\nmid\nfinish\n");
            assert!(!nix_marker.exists(), "native graph must not spawn nix");
            assert!(!wsl_marker.exists(), "native graph must not start WSL");
            for event in run_sink.events() {
                let kind = event_kind(event);
                assert!(
                    matches!(
                        kind,
                        "node_started" | "stdout_chunk" | "node_exited" | "run_completed"
                    ),
                    "unexpected event {kind}"
                );
                let encoded = serde_json::to_value(event).expect("serialize event");
                let restored: Event = serde_json::from_value(encoded).expect("deserialize event");
                assert_eq!(event_kind(&restored), kind);
            }
        }
    }

    #[cfg(unix)]
    fn write_trap(dir: &std::path::Path, name: &str, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join(name);
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        std::fs::write(&program, script).expect("write trap");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod trap");
    }

    #[test]
    fn flake_reference_is_a_flake_app_operation() {
        let document = parse(
            r#"
schema_version = 1

[tasks.fmt]
executor = { flake = "fmt" }
"#,
        );
        assert_eq!(
            document.tasks["fmt"].operation,
            Operation::FlakeApp {
                name: "fmt".to_owned()
            }
        );
    }

    #[test]
    fn explicit_unknown_root_is_not_found() {
        let document = parse(
            r#"
schema_version = 1

[tasks.prep]
executor = { native = ["/bin/true"] }
"#,
        );
        let error = plan_workflow_document(
            &document,
            &WorkflowPlanMode::Explicit {
                root: Some("missing".to_owned()),
            },
            FailurePolicy::FailFast,
            None,
        )
        .expect_err("unknown root");
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }
}
