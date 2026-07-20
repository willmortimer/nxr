//! Read-only ecosystem graph adapter boundary.
//!
//! Adapters in this module may **describe** project relationships and suggest
//! flake app names. They must never become operation authority: only
//! `apps.<system>.<name>` (via Nix) are executable leaf operations. See
//! `docs/ADAPTERS.md` in this repository.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current exploratory schema major for [`EcosystemGraph`] snapshots.
///
/// This is a documentation and boundary-testing surface only. It is not wired
/// into CLI discovery or execution in V2.3.
pub const ECOSYSTEM_GRAPH_SCHEMA_VERSION: u32 = 0;

/// Confidence assigned to an adapter-emitted relationship or suggestion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeConfidence {
    /// Declared directly in project metadata (for example `path` deps).
    Explicit,
    /// Inferred from conventional layout or tooling output.
    Inferred,
    /// Weak signal; must not silently drive destructive workflows.
    Low,
}

/// Kind of relationship between two graph nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Source package depends on target package.
    DependsOn,
    /// Source is a member of a workspace rooted at target.
    MemberOf,
    /// Source consumes generated output from target.
    GeneratedFrom,
}

/// A project or package node in an ecosystem graph snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// Stable node id (usually a repo-relative path).
    pub id: String,
    /// Human label for display.
    pub label: String,
    /// Optional coarse kind (for example `package`, `workspace_root`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// App names that *might* exist on the flake; hints only — not resolved or executed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_apps: Vec<String>,
}

/// Directed relationship between two nodes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub confidence: EdgeConfidence,
}

/// Versioned, read-only snapshot produced by an ecosystem graph adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EcosystemGraph {
    pub schema_version: u32,
    /// Adapter that produced this snapshot (for example `cargo-workspace`).
    pub adapter_id: String,
    /// Workspace root the adapter read (opaque path string).
    pub workspace_root: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<GraphNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<GraphEdge>,
}

impl EcosystemGraph {
    /// Parse a JSON snapshot and reject unsupported schema majors.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when JSON is invalid or `schema_version` is not
    /// [`ECOSYSTEM_GRAPH_SCHEMA_VERSION`].
    pub fn from_json(json: &str) -> Result<Self, AdapterError> {
        let graph: Self = serde_json::from_str(json)?;
        graph.validate_schema()?;
        Ok(graph)
    }

    /// Validate schema major and basic referential integrity.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the snapshot is internally inconsistent.
    pub fn validate_schema(&self) -> Result<(), AdapterError> {
        if self.schema_version != ECOSYSTEM_GRAPH_SCHEMA_VERSION {
            return Err(AdapterError::UnsupportedSchema {
                found: self.schema_version,
                expected: ECOSYSTEM_GRAPH_SCHEMA_VERSION,
            });
        }

        let node_ids: BTreeSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        if node_ids.len() != self.nodes.len() {
            return Err(AdapterError::DuplicateNodeId);
        }

        for edge in &self.edges {
            if !node_ids.contains(edge.from.as_str()) {
                return Err(AdapterError::UnknownEdgeEndpoint {
                    endpoint: edge.from.clone(),
                });
            }
            if !node_ids.contains(edge.to.as_str()) {
                return Err(AdapterError::UnknownEdgeEndpoint {
                    endpoint: edge.to.clone(),
                });
            }
        }

        Ok(())
    }

    /// Index nodes by id for lookup in tests and future inspect surfaces.
    #[must_use]
    pub fn nodes_by_id(&self) -> BTreeMap<&str, &GraphNode> {
        self.nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect()
    }
}

/// Errors from read-only ecosystem graph adapters.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AdapterError {
    #[error("unsupported ecosystem graph schema version {found} (expected {expected})")]
    UnsupportedSchema { found: u32, expected: u32 },

    #[error("duplicate node id in ecosystem graph snapshot")]
    DuplicateNodeId,

    #[error("edge references unknown node `{endpoint}`")]
    UnknownEdgeEndpoint { endpoint: String },

    #[error("failed to parse ecosystem graph JSON: {0}")]
    InvalidJson(String),
}

impl From<serde_json::Error> for AdapterError {
    fn from(source: serde_json::Error) -> Self {
        Self::InvalidJson(source.to_string())
    }
}

/// Read-only adapter boundary.
///
/// Implementations inspect adjacent project metadata and return
/// [`EcosystemGraph`] snapshots. They must not execute commands, install
/// toolchains, or define leaf operations. Executable authority remains with
/// flake apps discovered through the Nix adapter.
pub trait EcosystemGraphAdapter {
    /// Stable adapter identifier (for example `cargo-workspace`).
    fn adapter_id(&self) -> &str;

    /// Produce a read-only graph snapshot for `workspace_root`.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when metadata cannot be read or normalized.
    fn read_graph(&self, workspace_root: &str) -> Result<EcosystemGraph, AdapterError>;
}

/// Adapter that materializes a fixed JSON snapshot (documentation and tests).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticJsonAdapter {
    adapter_id: String,
    snapshot_json: String,
}

impl StaticJsonAdapter {
    /// Build an adapter from embedded or fixture JSON.
    #[must_use]
    pub fn new(adapter_id: impl Into<String>, snapshot_json: impl Into<String>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            snapshot_json: snapshot_json.into(),
        }
    }
}

impl EcosystemGraphAdapter for StaticJsonAdapter {
    fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    fn read_graph(&self, workspace_root: &str) -> Result<EcosystemGraph, AdapterError> {
        let mut graph = EcosystemGraph::from_json(&self.snapshot_json)?;
        if graph.adapter_id != self.adapter_id {
            return Err(AdapterError::InvalidJson(format!(
                "snapshot adapter_id `{}` does not match adapter `{}`",
                graph.adapter_id, self.adapter_id
            )));
        }
        workspace_root.clone_into(&mut graph.workspace_root);
        graph.validate_schema()?;
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterError, ECOSYSTEM_GRAPH_SCHEMA_VERSION, EcosystemGraph, EcosystemGraphAdapter,
        EdgeConfidence, EdgeKind, StaticJsonAdapter,
    };

    const CARGO_FIXTURE: &str =
        include_str!("../../../fixtures/ecosystem-graph-cargo/cargo-workspace-graph.json");

    #[test]
    fn cargo_fixture_parses_and_validates() {
        let graph = EcosystemGraph::from_json(CARGO_FIXTURE).expect("parse fixture");
        assert_eq!(graph.schema_version, ECOSYSTEM_GRAPH_SCHEMA_VERSION);
        assert_eq!(graph.adapter_id, "cargo-workspace");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
        assert!(
            graph
                .nodes_by_id()
                .get("crates/nxr-cli")
                .is_some_and(|node| node.suggested_apps == vec!["test".to_owned()])
        );
    }

    #[test]
    fn static_json_adapter_rebinds_workspace_root() {
        let adapter = StaticJsonAdapter::new("cargo-workspace", CARGO_FIXTURE);
        let graph = adapter
            .read_graph("/tmp/example")
            .expect("read graph from fixture");
        assert_eq!(graph.workspace_root, "/tmp/example");
        assert_eq!(adapter.adapter_id(), "cargo-workspace");
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let json = r#"{
            "schema_version": 99,
            "adapter_id": "cargo-workspace",
            "workspace_root": ".",
            "nodes": [],
            "edges": []
        }"#;
        let error = EcosystemGraph::from_json(json).expect_err("reject unknown major");
        assert!(matches!(
            error,
            AdapterError::UnsupportedSchema {
                found: 99,
                expected: ECOSYSTEM_GRAPH_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn unknown_edge_endpoint_is_rejected() {
        let json = r#"{
            "schema_version": 0,
            "adapter_id": "cargo-workspace",
            "workspace_root": ".",
            "nodes": [{ "id": "a", "label": "a" }],
            "edges": [{
                "from": "a",
                "to": "missing",
                "kind": "depends_on",
                "confidence": "explicit"
            }]
        }"#;
        let error = EcosystemGraph::from_json(json).expect_err("reject dangling edge");
        assert!(matches!(
            error,
            AdapterError::UnknownEdgeEndpoint { endpoint } if endpoint == "missing"
        ));
    }

    #[test]
    fn serde_round_trip_preserves_edge_kinds() {
        let graph = EcosystemGraph::from_json(CARGO_FIXTURE).expect("parse");
        let value = serde_json::to_value(&graph).expect("serialize");
        let restored: EcosystemGraph = serde_json::from_value(value).expect("deserialize");
        assert_eq!(restored, graph);
        assert_eq!(
            restored.edges.first().map(|edge| edge.kind),
            Some(EdgeKind::DependsOn)
        );
        assert_eq!(
            restored.edges.first().map(|edge| edge.confidence),
            Some(EdgeConfidence::Explicit)
        );
    }
}
