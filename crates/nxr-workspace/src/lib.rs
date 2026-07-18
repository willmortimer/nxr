//! Upward flake discovery, path normalization, and repository context.

pub mod discovery;
pub mod paths;

pub use discovery::{DiscoveryError, WorkspaceContext, discover_from};
