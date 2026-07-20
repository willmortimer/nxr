//! Conservative path-based affected analysis for flake apps and tasks.
//!
//! The heuristic prefers false positives over missed dependents: changes under
//! declared path roots, working directories, or flake/Nix inputs invalidate
//! matching nodes and propagate through task `dependsOn` edges.

mod analyze;
mod git;
mod graph;
mod paths;

pub use analyze::{AffectedAnalysis, AffectedError, AffectedNode, AffectedReason, analyze};
pub use git::{GitDiffError, git_diff_name_only};
pub use graph::{AffectedGraph, build_graph};
pub use paths::{is_global_invalidation_path, normalize_relative_path, path_matches_roots};
