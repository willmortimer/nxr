//! Run conservative affected analysis over a prepared graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use thiserror::Error;

use crate::graph::{AffectedGraph, NodeKind};
use crate::paths::{is_global_invalidation_path, path_matches_roots};

/// Errors during affected analysis.
#[derive(Debug, Error)]
pub enum AffectedError {
    /// No changed paths were supplied.
    #[error("no changed paths supplied; pass paths as arguments or use --base <git-ref>")]
    NoChangedPaths,
}

/// Why a node was marked affected.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AffectedReason {
    /// A changed path matched this node's declared roots.
    Path {
        /// Repository-relative changed path.
        path: String,
    },
    /// Flake/Nix input change invalidates all nodes.
    GlobalInput {
        /// Repository-relative changed path.
        path: String,
    },
    /// A dependent task or linked app was affected.
    Dependency {
        /// Name of the upstream affected node.
        from: String,
    },
    /// The backing app for this task was affected.
    App {
        /// App leaf name.
        app: String,
    },
}

/// One affected operation node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AffectedNode {
    pub kind: String,
    pub name: String,
    pub reasons: Vec<AffectedReason>,
}

/// Versioned affected-analysis result envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AffectedAnalysis {
    pub schema_version: u32,
    pub flake: String,
    pub system: String,
    pub changed_paths: Vec<String>,
    pub apps: Vec<String>,
    pub tasks: Vec<String>,
    pub nodes: Vec<AffectedNode>,
}

impl AffectedAnalysis {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// Analyze `changed_paths` against `graph`.
///
/// # Errors
///
/// Returns [`AffectedError`] when `changed_paths` is empty.
pub fn analyze(
    graph: &AffectedGraph,
    changed_paths: &[String],
    flake: &str,
    system: &str,
) -> Result<AffectedAnalysis, AffectedError> {
    if changed_paths.is_empty() {
        return Err(AffectedError::NoChangedPaths);
    }

    let mut reasons: BTreeMap<String, Vec<AffectedReason>> = BTreeMap::new();
    let global_change = changed_paths
        .iter()
        .any(|path| is_global_invalidation_path(path));

    if global_change {
        for (node_key, node) in &graph.nodes {
            let global_paths: Vec<_> = changed_paths
                .iter()
                .filter(|path| is_global_invalidation_path(path))
                .cloned()
                .collect();
            for path in global_paths {
                push_reason(&mut reasons, node_key, AffectedReason::GlobalInput { path });
            }
            let _ = node;
        }
    } else {
        for path in changed_paths {
            for (node_key, node) in &graph.nodes {
                if path_matches_roots(path, &node.path_roots) {
                    push_reason(
                        &mut reasons,
                        node_key,
                        AffectedReason::Path {
                            path: path.clone(),
                        },
                    );
                }
            }
        }
    }

    propagate_dependencies(graph, &mut reasons);

    let mut nodes = Vec::new();
    let mut apps = Vec::new();
    let mut tasks = Vec::new();

    for (node_key, node_reasons) in reasons {
        let Some(node) = graph.nodes.get(&node_key) else {
            continue;
        };
        match node.kind {
            NodeKind::App => apps.push(node.name.clone()),
            NodeKind::Task => tasks.push(node.name.clone()),
        }
        nodes.push(AffectedNode {
            kind: kind_label(node.kind).to_owned(),
            name: node.name.clone(),
            reasons: node_reasons,
        });
    }

    apps.sort();
    tasks.sort();
    nodes.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(AffectedAnalysis {
        schema_version: AffectedAnalysis::SCHEMA_VERSION,
        flake: flake.to_owned(),
        system: system.to_owned(),
        changed_paths: changed_paths.to_vec(),
        apps,
        tasks,
        nodes,
    })
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::App => "app",
        NodeKind::Task => "task",
    }
}

fn push_reason(reasons: &mut BTreeMap<String, Vec<AffectedReason>>, node_key: &str, reason: AffectedReason) {
    reasons.entry(node_key.to_owned()).or_default().push(reason);
}

fn propagate_dependencies(graph: &AffectedGraph, reasons: &mut BTreeMap<String, Vec<AffectedReason>>) {
    let mut queue: VecDeque<String> = reasons.keys().cloned().collect();
    let mut seen: BTreeSet<String> = reasons.keys().cloned().collect();

    while let Some(source_key) = queue.pop_front() {
        let Some(source_node) = graph.nodes.get(&source_key) else {
            continue;
        };
        let source_name = source_node.name.clone();

        if let Some(children) = graph.dependents.get(&source_key) {
            for child_key in children {
                if seen.insert(child_key.clone()) {
                    let from = match graph.nodes.get(&source_key).map(|node| node.kind) {
                        Some(NodeKind::App) => AffectedReason::App {
                            app: source_name.clone(),
                        },
                        Some(NodeKind::Task) | None => AffectedReason::Dependency {
                            from: source_name.clone(),
                        },
                    };
                    push_reason(reasons, child_key, from);
                    queue.push_back(child_key.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::App;
    use nxr_task::{TaskDefinition, TaskDocument};

    use super::analyze;
    use crate::graph::build_graph;

    fn shared_dep_fixture() -> (Vec<App>, TaskDocument) {
        let apps = vec![
            App {
                name: "shared-check".to_owned(),
                attr_path: "apps.aarch64-darwin.shared-check".to_owned(),
                flake_ref: "./fixtures/affected-deps".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "api-test".to_owned(),
                attr_path: "apps.aarch64-darwin.api-test".to_owned(),
                flake_ref: "./fixtures/affected-deps".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "web-test".to_owned(),
                attr_path: "apps.aarch64-darwin.web-test".to_owned(),
                flake_ref: "./fixtures/affected-deps".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "ci".to_owned(),
                attr_path: "apps.aarch64-darwin.ci".to_owned(),
                flake_ref: "./fixtures/affected-deps".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ];

        let mut tasks = BTreeMap::new();
        tasks.insert(
            "shared-lib".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: Vec::new(),
                app: "shared-check".to_owned(),
                working_directory: None,
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: vec!["shared".to_owned()],
            },
        );
        tasks.insert(
            "api-test".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: vec!["shared-lib".to_owned()],
                app: "api-test".to_owned(),
                working_directory: Some("crates/api".to_owned()),
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
            },
        );
        tasks.insert(
            "web-test".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: vec!["shared-lib".to_owned()],
                app: "web-test".to_owned(),
                working_directory: Some("crates/web".to_owned()),
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
            },
        );
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: vec!["api-test".to_owned(), "web-test".to_owned()],
                app: "ci".to_owned(),
                working_directory: None,
                hidden: false,
                category: None,
                aliases: Vec::new(),
                interactive: false,
                paths: Vec::new(),
            },
        );

        (apps, TaskDocument::new(tasks))
    }

    #[test]
    fn shared_dependency_change_affects_dependents() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["shared/lib.txt".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
        )
        .expect("analysis");

        assert!(result.tasks.contains(&"shared-lib".to_owned()));
        assert!(result.tasks.contains(&"api-test".to_owned()));
        assert!(result.tasks.contains(&"web-test".to_owned()));
        assert!(result.tasks.contains(&"ci".to_owned()));
        assert!(result.apps.is_empty());
    }

    #[test]
    fn nix_change_affects_all_nodes() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["flake.nix".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
        )
        .expect("analysis");

        assert_eq!(result.nodes.len(), 8);
    }

    #[test]
    fn local_path_change_does_not_affect_unrelated_tasks() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["crates/web/readme.md".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
        )
        .expect("analysis");

        assert_eq!(result.tasks, vec!["ci".to_owned(), "web-test".to_owned()]);
    }
}
