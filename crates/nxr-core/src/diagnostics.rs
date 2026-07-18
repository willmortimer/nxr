//! Runner exit codes and diagnostic helpers.

use serde::{Deserialize, Serialize};

/// Stable `nxr` process exit codes from the CLI contract.
pub mod exit {
    /// Successful execution or query.
    pub const SUCCESS: i32 = 0;
    /// Child operation failed when the exact status is unavailable.
    pub const CHILD_FAILED: i32 = 1;
    /// CLI usage error.
    pub const USAGE: i32 = 2;
    /// Flake discovery or resolution error.
    pub const DISCOVERY: i32 = 3;
    /// Nix capability or version error.
    pub const NIX_CAPABILITY: i32 = 4;
    /// Evaluation error.
    pub const EVALUATION: i32 = 5;
    /// App or task not found.
    pub const NOT_FOUND: i32 = 6;
    /// Invalid `nxr` metadata.
    pub const INVALID_METADATA: i32 = 7;
    /// Task graph planning error.
    pub const TASK_GRAPH: i32 = 8;
    /// Process supervision error.
    pub const PROCESS_SUPERVISION: i32 = 9;
    /// Interrupted before child status was available.
    pub const INTERRUPTED: i32 = 10;
}

/// Severity for runner-originated messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// Structured runner diagnostic with a stable machine-readable code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
}
