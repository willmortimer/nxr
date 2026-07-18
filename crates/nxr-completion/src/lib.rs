//! Shell completion scripts, dynamic candidates, and discovery cache.

pub mod cache;
pub mod dynamic;
pub mod generate;
pub mod shell;

pub use cache::{DiscoveryCacheOptions, DiscoveryContext, cached_apps, discover_with_cache};
pub use dynamic::{
    CompleteTarget, DISCOVERY_TIMEOUT, discover_app_candidates, write_app_candidates,
};
pub use generate::generate_script;
pub use shell::Shell;
