//! Filesystem watch and restart orchestration for nxr.

pub mod filter;
pub mod restart;
pub mod watcher;

pub use filter::{PathFilterError, PathFilters, should_ignore_path};
pub use restart::{Debouncer, Generation};
pub use watcher::{DEFAULT_DEBOUNCE, WatchConfig, WatchError, WatchPoll, WatchSession};
