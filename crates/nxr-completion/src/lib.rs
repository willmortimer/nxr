//! Shell completion scripts, dynamic candidates, and discovery cache.

pub mod cache;
pub mod dynamic;
pub mod fingerprint;
pub mod generate;
pub mod shell;

pub use cache::{
    CACHE_TTL_ENV, DEFAULT_CACHE_TTL_SECS, DiscoveryCacheEntry, DiscoveryCacheExplain,
    DiscoveryCacheMissReason, DiscoveryCacheOptions, DiscoveryCacheStatus, DiscoveryContext,
    WorkspaceDiscovery, cached_apps, cached_workspace, cached_workspace_best_effort,
    clear_discovery_cache, discover_with_cache, discover_workspace_with_cache, discovery_cache_dir,
    discovery_cache_entry, discovery_cache_entry_with_options, discovery_cache_status,
    explain_discovery_cache, gc_discovery_cache, hint_discovery_inputs_for_root,
    invalidate_discovery_cache,
};
pub use dynamic::{
    CompleteTarget, DISCOVERY_TIMEOUT, discover_app_candidates, write_app_candidates,
};
pub use fingerprint::{FINGERPRINT_IGNORE_ENV, discovery_inputs_fingerprint, nix_tree_fingerprint};
pub use generate::generate_script;
pub use shell::Shell;
