//! Shell completion scripts, dynamic candidates, and discovery cache.

pub mod cache;
pub mod dynamic;
pub mod fingerprint;
pub mod generate;
pub mod shell;

pub use cache::{
    DiscoveryCacheOptions, DiscoveryCacheStatus, DiscoveryContext, WorkspaceDiscovery,
    cached_apps, cached_workspace, clear_discovery_cache, discover_with_cache,
    discover_workspace_with_cache, discovery_cache_dir, discovery_cache_status,
};
pub use fingerprint::{FINGERPRINT_IGNORE_ENV, nix_tree_fingerprint};
pub use dynamic::{
    CompleteTarget, DISCOVERY_TIMEOUT, discover_app_candidates, write_app_candidates,
};
pub use generate::generate_script;
pub use shell::Shell;
