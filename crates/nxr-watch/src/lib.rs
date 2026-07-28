//! Filesystem watch and restart orchestration for nxr.

pub mod change;
pub mod filter;
pub mod restart;
pub mod snapshot;
pub mod watcher;

pub use change::{
    ChangeClass, MetadataInputRegistry, classify_pending_changes, classify_watch_path,
    merge_change_classes,
};
pub use filter::{PathFilterError, PathFilters, should_ignore_path};
pub use restart::{Debouncer, Generation};
pub use snapshot::{
    WATCH_SNAPSHOT_ENV, WatchIncrementalSnapshot, WatchSnapshotStats, WatchSourcePatch,
};
pub use watcher::{DEFAULT_DEBOUNCE, WatchConfig, WatchError, WatchPoll, WatchSession};
