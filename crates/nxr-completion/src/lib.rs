//! Shell completion scripts, dynamic candidates, and discovery cache.

pub mod cache;
pub mod dynamic;

pub use cache::{DiscoveryCacheOptions, DiscoveryContext, discover_with_cache};
