//! Member-qualified affected analysis across declared SourceSet members.
//!
//! A change is a `(member, path)` pair. Declared cross-member edges propagate
//! impact only to the named consumer operation, and only when the edge names a
//! path prefix that can be matched (or when the source member revision changes).
//! Incomplete edges set coverage to `partial` or `unknown` without blanketing
//! the consumer member.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::analyze::{AffectedReason, NodeStatus};
use crate::paths::{normalize_relative_path, path_matches_roots};

/// One member-qualified path change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemberChange {
    /// SourceSet member that owns the path.
    pub member: String,
    /// Repository-relative path within that member.
    ///
    /// Empty or the token `REVISION` means the member revision changed.
    pub path: String,
}

/// One operation node owned by a SourceSet member.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemberNode {
    /// SourceSet member that declares this operation.
    pub member: String,
    /// Operation name (app or task leaf name).
    pub name: String,
    /// Declared path roots within the member checkout.
    pub path_roots: Vec<String>,
}

/// Declared cross-member dependency edge, owned by the consumer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemberEdge {
    /// Member that consumes the dependency.
    pub consumer_member: String,
    /// Operation name on the consumer member.
    pub consumer_name: String,
    /// Member that supplies the dependency.
    pub source_member: String,
    /// Optional path prefix within the source member.
    ///
    /// `None` means the relationship is declared but path coverage is unknown.
    pub source_path_prefix: Option<String>,
}

/// One classified member-qualified operation node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemberAffectedNode {
    pub member: String,
    pub name: String,
    pub status: NodeStatus,
    pub reasons: Vec<AffectedReason>,
}

/// Result of member-qualified affected analysis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemberAffectedAnalysis {
    pub schema_version: u32,
    /// Aggregate edge coverage: `complete`, `partial`, or `unknown`.
    pub coverage: String,
    pub changes: Vec<MemberChange>,
    pub nodes: Vec<MemberAffectedNode>,
}

impl MemberAffectedAnalysis {
    pub const SCHEMA_VERSION: u32 = 1;
}

const COVERAGE_COMPLETE: &str = "complete";
const COVERAGE_PARTIAL: &str = "partial";
const COVERAGE_UNKNOWN: &str = "unknown";

/// Analyze member-qualified changes against in-memory nodes and declared edges.
///
/// Same-member path ownership uses [`path_matches_roots`]. Cross-member impact
/// follows only the named consumer on each edge. An edge without a resolvable
/// source member or path prefix contributes `partial` / `unknown` coverage and
/// does not mark every operation of the consumer member affected.
#[must_use]
pub fn analyze_members(
    nodes: &[MemberNode],
    edges: &[MemberEdge],
    changes: &[MemberChange],
) -> MemberAffectedAnalysis {
    let known_members = known_members(nodes);
    let coverage = edge_coverage(edges, &known_members);

    let mut status: BTreeMap<NodeKey, NodeStatus> = BTreeMap::new();
    let mut reasons: BTreeMap<NodeKey, Vec<AffectedReason>> = BTreeMap::new();

    for node in nodes {
        status.insert(node_key(node), NodeStatus::Unaffected);
    }

    if !changes.is_empty() {
        classify_member_hits(nodes, changes, &mut status, &mut reasons);
        propagate_member_edges(
            nodes,
            edges,
            changes,
            &known_members,
            &mut status,
            &mut reasons,
        );
        finalize_empty_roots(nodes, changes, &mut status, &mut reasons);
    }

    let mut result_nodes = Vec::with_capacity(nodes.len());
    for node in nodes {
        let key = node_key(node);
        let node_status = status.get(&key).copied().unwrap_or(NodeStatus::Unknown);
        let node_reasons = reasons.remove(&key).unwrap_or_default();
        result_nodes.push(MemberAffectedNode {
            member: node.member.clone(),
            name: node.name.clone(),
            status: node_status,
            reasons: node_reasons,
        });
    }
    result_nodes.sort_by(|left, right| {
        left.member
            .cmp(&right.member)
            .then(left.name.cmp(&right.name))
    });

    MemberAffectedAnalysis {
        schema_version: MemberAffectedAnalysis::SCHEMA_VERSION,
        coverage,
        changes: changes.to_vec(),
        nodes: result_nodes,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct NodeKey {
    member: String,
    name: String,
}

fn node_key(node: &MemberNode) -> NodeKey {
    NodeKey {
        member: node.member.clone(),
        name: node.name.clone(),
    }
}

fn key_of(member: &str, name: &str) -> NodeKey {
    NodeKey {
        member: member.to_owned(),
        name: name.to_owned(),
    }
}

fn known_members(nodes: &[MemberNode]) -> BTreeSet<String> {
    nodes.iter().map(|node| node.member.clone()).collect()
}

fn edge_coverage(edges: &[MemberEdge], known_members: &BTreeSet<String>) -> String {
    let mut coverage = COVERAGE_COMPLETE;
    for edge in edges {
        if !known_members.contains(&edge.source_member) {
            coverage = COVERAGE_UNKNOWN;
            continue;
        }
        if edge.source_path_prefix.is_none() && coverage != COVERAGE_UNKNOWN {
            coverage = COVERAGE_PARTIAL;
        }
    }
    coverage.to_owned()
}

fn is_revision_change(path: &str) -> bool {
    let normalized = normalize_relative_path(path);
    normalized.is_empty() || normalized == "REVISION"
}

fn member_has_changes(member: &str, changes: &[MemberChange]) -> bool {
    changes.iter().any(|change| change.member == member)
}

fn classify_member_hits(
    nodes: &[MemberNode],
    changes: &[MemberChange],
    status: &mut BTreeMap<NodeKey, NodeStatus>,
    reasons: &mut BTreeMap<NodeKey, Vec<AffectedReason>>,
) {
    for node in nodes {
        let key = node_key(node);
        let member_changes: Vec<_> = changes
            .iter()
            .filter(|change| change.member == node.member)
            .collect();
        if member_changes.is_empty() {
            continue;
        }

        let mut matched = false;
        let mut revision = false;
        let mut invalid = false;
        for change in &member_changes {
            if is_revision_change(&change.path) {
                revision = true;
                push_reason(
                    reasons,
                    &key,
                    AffectedReason::Path {
                        path: "REVISION".to_owned(),
                    },
                );
                continue;
            }
            if node.path_roots.is_empty() {
                continue;
            }
            match path_matches_roots(&change.path, &node.path_roots) {
                Ok(true) => {
                    push_reason(
                        reasons,
                        &key,
                        AffectedReason::Path {
                            path: normalize_relative_path(&change.path),
                        },
                    );
                    matched = true;
                }
                Ok(false) => {}
                Err(error) => {
                    let pattern = match error {
                        crate::paths::PathRootError::InvalidGlob { pattern, .. } => pattern,
                        crate::paths::PathRootError::InvalidRepoPath { path } => path,
                    };
                    push_reason(reasons, &key, AffectedReason::InvalidGlob { pattern });
                    invalid = true;
                    break;
                }
            }
        }

        if invalid {
            status.insert(key, NodeStatus::Unknown);
        } else if revision || matched {
            status.insert(key, NodeStatus::Affected);
        }
    }
}

fn propagate_member_edges(
    nodes: &[MemberNode],
    edges: &[MemberEdge],
    changes: &[MemberChange],
    known_members: &BTreeSet<String>,
    status: &mut BTreeMap<NodeKey, NodeStatus>,
    reasons: &mut BTreeMap<NodeKey, Vec<AffectedReason>>,
) {
    let node_index: BTreeSet<NodeKey> = nodes.iter().map(node_key).collect();

    for edge in edges {
        let consumer = key_of(&edge.consumer_member, &edge.consumer_name);
        if !node_index.contains(&consumer) {
            continue;
        }
        if !known_members.contains(&edge.source_member) {
            // Unknown source member: coverage already unknown; do not blanket
            // the consumer member.
            continue;
        }

        let source_changes: Vec<_> = changes
            .iter()
            .filter(|change| change.member == edge.source_member)
            .collect();
        if source_changes.is_empty() {
            continue;
        }

        let revision_hit = source_changes
            .iter()
            .any(|change| is_revision_change(&change.path));
        if revision_hit {
            push_reason(
                reasons,
                &consumer,
                AffectedReason::Dependency {
                    from: edge.source_member.clone(),
                },
            );
            status.insert(consumer.clone(), NodeStatus::Affected);
            continue;
        }

        let Some(prefix) = edge.source_path_prefix.as_deref() else {
            // The named consumer declared the edge, but the path is not known.
            // Mark that one node unknown. Do not blanket the rest of its member.
            if status.get(&consumer) != Some(&NodeStatus::Affected) {
                push_reason(
                    reasons,
                    &consumer,
                    AffectedReason::UnknownDependency {
                        from: edge.source_member.clone(),
                    },
                );
                status.insert(consumer.clone(), NodeStatus::Unknown);
            }
            continue;
        };
        let roots = [normalize_relative_path(prefix)];
        for change in source_changes {
            if is_revision_change(&change.path) {
                continue;
            }
            match path_matches_roots(&change.path, &roots) {
                Ok(true) => {
                    push_reason(
                        reasons,
                        &consumer,
                        AffectedReason::Dependency {
                            from: format!(
                                "{}:{}",
                                edge.source_member,
                                normalize_relative_path(&change.path)
                            ),
                        },
                    );
                    status.insert(consumer.clone(), NodeStatus::Affected);
                }
                Ok(false) => {}
                Err(_) => {
                    // Unmatchable prefix keeps coverage incomplete already; do
                    // not blanket-affect the consumer member.
                }
            }
        }
    }
}

fn finalize_empty_roots(
    nodes: &[MemberNode],
    changes: &[MemberChange],
    status: &mut BTreeMap<NodeKey, NodeStatus>,
    reasons: &mut BTreeMap<NodeKey, Vec<AffectedReason>>,
) {
    for node in nodes {
        let key = node_key(node);
        if status.get(&key) == Some(&NodeStatus::Affected) {
            continue;
        }
        if node.path_roots.is_empty() && member_has_changes(&node.member, changes) {
            // Revision already marked affected; remaining empty-root cases are
            // ownership-unknown for non-revision member changes.
            let has_non_revision = changes
                .iter()
                .any(|change| change.member == node.member && !is_revision_change(&change.path));
            if has_non_revision {
                push_reason(reasons, &key, AffectedReason::EmptyRoots);
                status.insert(key, NodeStatus::Unknown);
            }
        }
    }
}

fn push_reason(
    reasons: &mut BTreeMap<NodeKey, Vec<AffectedReason>>,
    key: &NodeKey,
    reason: AffectedReason,
) {
    let entry = reasons.entry(key.clone()).or_default();
    if !entry.contains(&reason) {
        entry.push(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::{MemberChange, MemberEdge, MemberNode, analyze_members};
    use crate::analyze::NodeStatus;

    fn lattice_stargate_fixture() -> (Vec<MemberNode>, Vec<MemberEdge>) {
        let nodes = vec![
            MemberNode {
                member: "lattice".to_owned(),
                name: "build-integration".to_owned(),
                path_roots: vec!["crates/lattice".to_owned()],
            },
            MemberNode {
                member: "lattice".to_owned(),
                name: "unrelated-lint".to_owned(),
                path_roots: vec!["crates/lint".to_owned()],
            },
            MemberNode {
                member: "stargate".to_owned(),
                name: "typecheck".to_owned(),
                path_roots: vec!["packages/api".to_owned()],
            },
            MemberNode {
                member: "stargate".to_owned(),
                name: "docs".to_owned(),
                path_roots: vec!["docs".to_owned()],
            },
        ];
        let edges = vec![MemberEdge {
            consumer_member: "lattice".to_owned(),
            consumer_name: "build-integration".to_owned(),
            source_member: "stargate".to_owned(),
            source_path_prefix: Some("packages/api".to_owned()),
        }];
        (nodes, edges)
    }

    fn status_of<'a>(
        result: &'a super::MemberAffectedAnalysis,
        member: &str,
        name: &str,
    ) -> &'a NodeStatus {
        &result
            .nodes
            .iter()
            .find(|node| node.member == member && node.name == name)
            .expect("node present")
            .status
    }

    #[test]
    fn lattice_only_file_affects_matching_lattice_operation() {
        let (nodes, edges) = lattice_stargate_fixture();
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "lattice".to_owned(),
                path: "crates/lattice/src/lib.rs".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "complete");
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Unaffected
        );
        assert_eq!(
            *status_of(&result, "stargate", "docs"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn stargate_dependency_file_affects_declared_lattice_edge_only() {
        let (nodes, edges) = lattice_stargate_fixture();
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "stargate".to_owned(),
                path: "packages/api/src/index.ts".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "complete");
        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "stargate", "docs"),
            NodeStatus::Unaffected
        );
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn unrelated_third_member_affects_neither_lattice_nor_stargate() {
        let (nodes, edges) = lattice_stargate_fixture();
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "cell".to_owned(),
                path: "src/lib.rs".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "complete");
        assert!(
            result
                .nodes
                .iter()
                .all(|node| node.status == NodeStatus::Unaffected)
        );
    }

    #[test]
    fn stargate_revision_invalidates_dependents_not_unrelated_lattice() {
        let (nodes, edges) = lattice_stargate_fixture();
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "stargate".to_owned(),
                path: "REVISION".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "complete");
        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "stargate", "docs"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn incomplete_edge_sets_partial_coverage_without_blanketing_consumer() {
        let (nodes, _) = lattice_stargate_fixture();
        let edges = vec![MemberEdge {
            consumer_member: "lattice".to_owned(),
            consumer_name: "build-integration".to_owned(),
            source_member: "stargate".to_owned(),
            source_path_prefix: None,
        }];
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "stargate".to_owned(),
                path: "packages/api/src/index.ts".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "partial");
        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Unknown
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn unknown_source_member_edge_sets_unknown_coverage_without_blanketing() {
        let (nodes, _) = lattice_stargate_fixture();
        let edges = vec![MemberEdge {
            consumer_member: "lattice".to_owned(),
            consumer_name: "build-integration".to_owned(),
            source_member: "missing".to_owned(),
            source_path_prefix: Some("packages/api".to_owned()),
        }];
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "stargate".to_owned(),
                path: "packages/api/src/index.ts".to_owned(),
            }],
        );

        assert_eq!(result.coverage, "unknown");
        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Unaffected
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn empty_path_revision_token_matches_revision_semantics() {
        let (nodes, edges) = lattice_stargate_fixture();
        let result = analyze_members(
            &nodes,
            &edges,
            &[MemberChange {
                member: "stargate".to_owned(),
                path: String::new(),
            }],
        );

        assert_eq!(
            *status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "build-integration"),
            NodeStatus::Affected
        );
        assert_eq!(
            *status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }
}
