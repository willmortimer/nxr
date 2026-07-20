//! Run conservative affected analysis over a prepared graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::graph::{AffectedGraph, NodeKind};
use crate::paths::{
    PathRootError, is_global_invalidation_path, path_matches_roots, validate_path_roots,
};

/// Classification of a graph node relative to the changed paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Path ownership matched a change, or a dependency forced inclusion.
    Affected,
    /// Declared roots exist and do not overlap the change set.
    Unaffected,
    /// Ownership cannot be decided (empty roots, invalid globs, or unknown deps).
    Unknown,
}

/// Why a node was marked affected or unknown.
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
    /// Declared path roots include an invalid glob pattern.
    InvalidGlob {
        /// The invalid pattern.
        pattern: String,
    },
    /// Node has no path roots, so ownership is unknown.
    EmptyRoots,
    /// An upstream dependency could not be classified.
    UnknownDependency {
        /// Name of the upstream unknown node.
        from: String,
    },
}

/// One classified operation node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AffectedNode {
    pub kind: String,
    pub name: String,
    pub status: NodeStatus,
    pub reasons: Vec<AffectedReason>,
}

/// Versioned affected-analysis result envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AffectedAnalysis {
    pub schema_version: u32,
    pub flake: String,
    pub system: String,
    pub strict: bool,
    pub changed_paths: Vec<String>,
    pub apps: Vec<String>,
    pub tasks: Vec<String>,
    pub nodes: Vec<AffectedNode>,
}

impl AffectedAnalysis {
    pub const SCHEMA_VERSION: u32 = 2;
}

/// Analyze `changed_paths` against `graph`.
///
/// When `strict` is true (default for CI JSON), `apps` / `tasks` include both
/// [`NodeStatus::Affected`] and [`NodeStatus::Unknown`]. Only
/// [`NodeStatus::Unaffected`] is omitted from those lists.
///
/// An empty `changed_paths` slice is valid and yields a successful analysis
/// (typically path-rooted nodes [`NodeStatus::Unaffected`], empty-root nodes
/// [`NodeStatus::Unknown`]). Callers that require at least one path *source*
/// should enforce that before invoking analysis.
#[must_use]
pub fn analyze(
    graph: &AffectedGraph,
    changed_paths: &[String],
    flake: &str,
    system: &str,
    strict: bool,
) -> AffectedAnalysis {
    let mut status: BTreeMap<String, NodeStatus> = BTreeMap::new();
    let mut reasons: BTreeMap<String, Vec<AffectedReason>> = BTreeMap::new();

    let global_change = changed_paths
        .iter()
        .any(|path| is_global_invalidation_path(path));

    if global_change {
        mark_global_affected(graph, changed_paths, &mut status, &mut reasons);
    } else {
        classify_path_hits(graph, changed_paths, &mut status, &mut reasons);
    }

    propagate_dependencies(graph, &mut status, &mut reasons);

    if !global_change {
        finalize_empty_roots(graph, &mut status, &mut reasons);
        propagate_dependencies(graph, &mut status, &mut reasons);
    }

    let (apps, tasks, nodes) = collect_results(graph, &status, &mut reasons, strict);

    AffectedAnalysis {
        schema_version: AffectedAnalysis::SCHEMA_VERSION,
        flake: flake.to_owned(),
        system: system.to_owned(),
        strict,
        changed_paths: changed_paths.to_vec(),
        apps,
        tasks,
        nodes,
    }
}

fn mark_global_affected(
    graph: &AffectedGraph,
    changed_paths: &[String],
    status: &mut BTreeMap<String, NodeStatus>,
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
) {
    let global_paths: Vec<_> = changed_paths
        .iter()
        .filter(|path| is_global_invalidation_path(path))
        .cloned()
        .collect();
    for node_key in graph.nodes.keys() {
        for path in &global_paths {
            push_reason(
                reasons,
                node_key,
                AffectedReason::GlobalInput { path: path.clone() },
            );
        }
        status.insert(node_key.clone(), NodeStatus::Affected);
    }
}

fn classify_path_hits(
    graph: &AffectedGraph,
    changed_paths: &[String],
    status: &mut BTreeMap<String, NodeStatus>,
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
) {
    for (node_key, node) in &graph.nodes {
        if let Err(error) = validate_path_roots(&node.path_roots) {
            let PathRootError::InvalidGlob { pattern, .. } = error;
            push_reason(reasons, node_key, AffectedReason::InvalidGlob { pattern });
            status.insert(node_key.clone(), NodeStatus::Unknown);
            continue;
        }

        // Empty roots stay unclassified until after affected propagation so a
        // dependency hit can mark them affected without an EmptyRoots reason.
        if node.path_roots.is_empty() {
            continue;
        }

        match match_changed_paths(node_key, &node.path_roots, changed_paths, reasons) {
            PathMatchOutcome::Matched => {
                status.insert(node_key.clone(), NodeStatus::Affected);
            }
            PathMatchOutcome::Missed => {
                status.insert(node_key.clone(), NodeStatus::Unaffected);
            }
            PathMatchOutcome::Invalid => {
                status.insert(node_key.clone(), NodeStatus::Unknown);
            }
        }
    }
}

enum PathMatchOutcome {
    Matched,
    Missed,
    Invalid,
}

fn match_changed_paths(
    node_key: &str,
    path_roots: &[String],
    changed_paths: &[String],
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
) -> PathMatchOutcome {
    let mut matched = false;
    for path in changed_paths {
        match path_matches_roots(path, path_roots) {
            Ok(true) => {
                push_reason(
                    reasons,
                    node_key,
                    AffectedReason::Path { path: path.clone() },
                );
                matched = true;
            }
            Ok(false) => {}
            Err(error) => {
                let PathRootError::InvalidGlob { pattern, .. } = error;
                push_reason(reasons, node_key, AffectedReason::InvalidGlob { pattern });
                return PathMatchOutcome::Invalid;
            }
        }
    }
    if matched {
        PathMatchOutcome::Matched
    } else {
        PathMatchOutcome::Missed
    }
}

fn finalize_empty_roots(
    graph: &AffectedGraph,
    status: &mut BTreeMap<String, NodeStatus>,
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
) {
    for (node_key, node) in &graph.nodes {
        if status.contains_key(node_key) {
            continue;
        }
        if node.path_roots.is_empty() {
            push_reason(reasons, node_key, AffectedReason::EmptyRoots);
            status.insert(node_key.clone(), NodeStatus::Unknown);
        } else {
            status.insert(node_key.clone(), NodeStatus::Unaffected);
        }
    }
}

fn collect_results(
    graph: &AffectedGraph,
    status: &BTreeMap<String, NodeStatus>,
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
    strict: bool,
) -> (Vec<String>, Vec<String>, Vec<AffectedNode>) {
    let mut nodes = Vec::new();
    let mut apps = Vec::new();
    let mut tasks = Vec::new();

    for (node_key, node) in &graph.nodes {
        let node_status = status.get(node_key).copied().unwrap_or(NodeStatus::Unknown);
        let node_reasons = reasons.remove(node_key).unwrap_or_default();

        if include_in_lists(node_status, strict) {
            match node.kind {
                NodeKind::App => apps.push(node.name.clone()),
                NodeKind::Task => tasks.push(node.name.clone()),
            }
        }

        nodes.push(AffectedNode {
            kind: kind_label(node.kind).to_owned(),
            name: node.name.clone(),
            status: node_status,
            reasons: node_reasons,
        });
    }

    apps.sort();
    tasks.sort();
    nodes.sort_by(|left, right| left.name.cmp(&right.name).then(left.kind.cmp(&right.kind)));
    (apps, tasks, nodes)
}

fn include_in_lists(status: NodeStatus, strict: bool) -> bool {
    match status {
        NodeStatus::Affected => true,
        NodeStatus::Unknown => strict,
        NodeStatus::Unaffected => false,
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::App => "app",
        NodeKind::Task => "task",
    }
}

fn push_reason(
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
    node_key: &str,
    reason: AffectedReason,
) {
    let entry = reasons.entry(node_key.to_owned()).or_default();
    if !entry.contains(&reason) {
        entry.push(reason);
    }
}

fn propagate_dependencies(
    graph: &AffectedGraph,
    status: &mut BTreeMap<String, NodeStatus>,
    reasons: &mut BTreeMap<String, Vec<AffectedReason>>,
) {
    let mut queue: VecDeque<String> = status
        .iter()
        .filter(|(_, value)| matches!(value, NodeStatus::Affected | NodeStatus::Unknown))
        .map(|(key, _)| key.clone())
        .collect();
    let mut enqueued: BTreeSet<String> = queue.iter().cloned().collect();

    while let Some(source_key) = queue.pop_front() {
        let Some(source_node) = graph.nodes.get(&source_key) else {
            continue;
        };
        let source_name = source_node.name.clone();
        let source_status = status
            .get(&source_key)
            .copied()
            .unwrap_or(NodeStatus::Unknown);

        let Some(children) = graph.dependents.get(&source_key) else {
            continue;
        };

        for child_key in children {
            let child_status = status.get(child_key).copied();

            match source_status {
                NodeStatus::Affected => {
                    let reason = dependency_reason(graph, &source_key, &source_name);
                    push_reason(reasons, child_key, reason);
                    if child_status == Some(NodeStatus::Affected) {
                        continue;
                    }
                    status.insert(child_key.clone(), NodeStatus::Affected);
                    if enqueued.insert(child_key.clone()) {
                        queue.push_back(child_key.clone());
                    }
                }
                NodeStatus::Unknown => {
                    // Only task→task unknown edges propagate. Unknown apps
                    // (typical when listing metadata omits paths) must not
                    // poison every task that merely runs them.
                    if source_node.kind != NodeKind::Task {
                        continue;
                    }
                    if child_status == Some(NodeStatus::Affected) {
                        continue;
                    }
                    push_reason(
                        reasons,
                        child_key,
                        AffectedReason::UnknownDependency {
                            from: source_name.clone(),
                        },
                    );
                    if child_status == Some(NodeStatus::Unknown) {
                        continue;
                    }
                    status.insert(child_key.clone(), NodeStatus::Unknown);
                    if enqueued.insert(child_key.clone()) {
                        queue.push_back(child_key.clone());
                    }
                }
                NodeStatus::Unaffected => {}
            }
        }
    }
}

fn dependency_reason(graph: &AffectedGraph, source_key: &str, source_name: &str) -> AffectedReason {
    match graph.nodes.get(source_key).map(|node| node.kind) {
        Some(NodeKind::App) => AffectedReason::App {
            app: source_name.to_owned(),
        },
        Some(NodeKind::Task) | None => AffectedReason::Dependency {
            from: source_name.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::App;
    use nxr_task::{TaskDefinition, TaskDocument};

    use super::{AffectedReason, NodeStatus, analyze};
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
            true,
        );

        assert!(result.tasks.contains(&"shared-lib".to_owned()));
        assert!(result.tasks.contains(&"api-test".to_owned()));
        assert!(result.tasks.contains(&"web-test".to_owned()));
        assert!(result.tasks.contains(&"ci".to_owned()));
        // Apps lack path roots → unknown, included under strict.
        assert!(result.apps.contains(&"shared-check".to_owned()));
        assert!(result.strict);
        assert_eq!(result.schema_version, 2);
    }

    #[test]
    fn non_strict_omits_unknown_apps() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["shared/lib.txt".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
            false,
        );

        assert!(result.apps.is_empty());
        assert!(result.tasks.contains(&"shared-lib".to_owned()));
        assert!(!result.strict);
        let unknown_apps = result
            .nodes
            .iter()
            .filter(|node| node.kind == "app" && node.status == NodeStatus::Unknown)
            .count();
        assert_eq!(unknown_apps, 4);
    }

    #[test]
    fn empty_changed_paths_succeeds_with_classification() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &[],
            "./fixtures/affected-deps",
            "aarch64-darwin",
            true,
        );

        assert!(result.changed_paths.is_empty());
        assert_eq!(result.nodes.len(), graph.nodes.len());
        let unknown = result
            .nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Unknown)
            .count();
        let unaffected = result
            .nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Unaffected)
            .count();
        assert!(unknown > 0);
        assert!(unaffected > 0);
    }

    #[test]
    fn empty_path_roots_classify_as_unknown() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["crates/web/readme.md".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
            true,
        );

        let ci_app = result
            .nodes
            .iter()
            .find(|node| node.kind == "app" && node.name == "ci")
            .expect("ci app");
        assert_eq!(ci_app.status, NodeStatus::Unknown);
        assert!(ci_app.reasons.contains(&AffectedReason::EmptyRoots));
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
            true,
        );

        assert_eq!(result.nodes.len(), 8);
        assert!(
            result
                .nodes
                .iter()
                .all(|node| node.status == NodeStatus::Affected)
        );
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
            false,
        );

        assert_eq!(result.tasks, vec!["ci".to_owned(), "web-test".to_owned()]);
        assert!(!result.tasks.contains(&"api-test".to_owned()));
        assert!(!result.tasks.contains(&"shared-lib".to_owned()));
    }

    #[test]
    fn diamond_propagates_all_dependency_reasons() {
        let (apps, task_doc) = shared_dep_fixture();
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["shared/lib.txt".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
            false,
        );

        let ci = result
            .nodes
            .iter()
            .find(|node| node.kind == "task" && node.name == "ci")
            .expect("ci task");
        assert!(ci.reasons.contains(&AffectedReason::Dependency {
            from: "api-test".to_owned()
        }));
        assert!(ci.reasons.contains(&AffectedReason::Dependency {
            from: "web-test".to_owned()
        }));
    }

    #[test]
    fn invalid_glob_marks_node_unknown() {
        let apps = vec![App {
            name: "broken".to_owned(),
            attr_path: "apps.aarch64-darwin.broken".to_owned(),
            flake_ref: "./fixtures/affected-deps".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: None,
            is_default: false,
            metadata: {
                let mut metadata = BTreeMap::new();
                metadata.insert("paths".to_owned(), serde_json::json!(["crates/[broken"]));
                metadata
            },
        }];
        let task_doc = TaskDocument::new(BTreeMap::new());
        let graph = build_graph(&apps, &task_doc);
        let result = analyze(
            &graph,
            &["crates/broken/lib.rs".to_owned()],
            "./fixtures/affected-deps",
            "aarch64-darwin",
            true,
        );

        assert_eq!(result.apps, vec!["broken".to_owned()]);
        let node = &result.nodes[0];
        assert_eq!(node.status, NodeStatus::Unknown);
        assert!(matches!(
            &node.reasons[0],
            AffectedReason::InvalidGlob { pattern } if pattern == "crates/[broken"
        ));
    }
}
