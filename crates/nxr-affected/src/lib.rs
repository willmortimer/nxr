//! Conservative path-based affected analysis for flake apps and tasks.
//!
//! The heuristic prefers false positives over missed dependents: changes under
//! declared path roots, working directories, or flake/Nix inputs invalidate
//! matching nodes and propagate through task `dependsOn` edges. Nodes without
//! path ownership are classified `unknown` and included under the default
//! strict CI policy.

mod analyze;
mod git;
mod graph;
mod paths;

pub use analyze::{
    AffectedAnalysis, AffectedError, AffectedNode, AffectedReason, NodeStatus, analyze,
};
pub use git::{GitDiffError, git_all_changes, git_diff_name_only, git_working_tree_changes};
pub use graph::{AffectedGraph, build_graph};
pub use paths::{
    PathRootError, is_global_invalidation_path, looks_like_glob, normalize_relative_path,
    path_matches_roots, validate_path_roots,
};
