//! Filesystem watch and restart orchestration for nxr.

pub mod restart;
pub mod watcher;

pub use restart::{Debouncer, Generation};
pub use watcher::{
    DEFAULT_DEBOUNCE, WatchConfig, WatchError, WatchPoll, WatchSession, should_ignore_path,
};
