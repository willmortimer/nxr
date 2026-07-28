//! Filesystem watch and restart orchestration for nxr.

pub mod change;
pub mod coalesce;
pub mod filter;
pub mod prewarm;
pub mod restart;
pub mod snapshot;
pub mod watcher;

pub use change::{
    ChangeClass, MetadataInputRegistry, classify_pending_changes, classify_watch_path,
    merge_change_classes,
};
pub use coalesce::{WATCH_COALESCE_ENV, WatchCoalesceStats, WatchSemanticCoalescer};
pub use filter::{PathFilterError, PathFilters, should_ignore_path};
pub use prewarm::{
    PrewarmCasHandle, PrewarmContext, PrewarmStoreExe, WATCH_PREWARM_ENV, WatchOwnershipIndex,
    WatchPrewarm, WatchPrewarmStats,
};
pub use restart::{Debouncer, Generation};
pub use snapshot::{
    WATCH_SNAPSHOT_ENV, WatchIncrementalSnapshot, WatchSnapshotStats, WatchSourcePatch,
};
pub use watcher::{DEFAULT_DEBOUNCE, WatchConfig, WatchError, WatchPoll, WatchSession};
