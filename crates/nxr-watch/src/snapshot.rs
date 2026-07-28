//! Incremental workspace snapshot for watch mode ([ADR-0160]).
//!
//! Retains run-scoped digest / Merkle state across watch generations so source
//! events patch file metadata and directory digests instead of cold rescans.
//! Wave 5b (semantic coalesce) and 5c (prewarm) extend via reserved hooks.

use std::collections::BTreeSet;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_affected::{AffectedGraph, NodeStatus, analyze, build_graph, normalize_relative_path};
use nxr_core::{
    App, RunDigestCache, record_watch_paths_invalidated, record_watch_prepared_nodes_dropped,
    record_watch_snapshot_patch,
};
use nxr_task::TaskDocument;

/// Kill-switch for watch incremental snapshot (`off` / `0` / `false` / `no`).
pub const WATCH_SNAPSHOT_ENV: &str = "NXR_WATCH_SNAPSHOT";

/// Outcome of patching the snapshot for one source-only restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchSourcePatch {
    /// Normalized repo-relative paths applied to digest / Merkle indexes.
    pub paths: Vec<String>,
    /// Execution-plan task ids whose prepared nodes should be dropped.
    pub affected_plan_nodes: Vec<String>,
    /// Durable action-digest entries removed.
    pub action_digest_entries_removed: usize,
}

/// Cumulative watch snapshot statistics (also mirrored in `NXR_PERF_STATS`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchSnapshotStats {
    pub source_patches: u64,
    pub paths_invalidated: u64,
    pub prepared_nodes_dropped: u64,
}

/// In-memory workspace snapshot retained for the watch session.
#[derive(Debug)]
pub struct WatchIncrementalSnapshot {
    root: Utf8PathBuf,
    digest_cache: RunDigestCache,
    graph: Option<AffectedGraph>,
    stats: WatchSnapshotStats,
    /// Reserved for Wave 5b semantic coalesce (formatter storms, rename pairs).
    #[allow(dead_code)]
    coalesce_hook: (),
    /// Reserved for Wave 5c prewarm (resolved exe / CAS handles).
    #[allow(dead_code)]
    prewarm_hook: (),
}

impl WatchIncrementalSnapshot {
    /// Create an empty snapshot for `root`.
    #[must_use]
    pub fn new(root: Utf8PathBuf) -> Self {
        Self {
            root,
            digest_cache: RunDigestCache::new(),
            graph: None,
            stats: WatchSnapshotStats::default(),
            coalesce_hook: (),
            prewarm_hook: (),
        }
    }

    /// Whether incremental watch snapshot is enabled (default on).
    #[must_use]
    pub fn enabled() -> bool {
        match std::env::var(WATCH_SNAPSHOT_ENV) {
            Ok(value) => {
                let normalized = value.trim();
                if normalized.is_empty() {
                    return true;
                }
                !matches!(
                    normalized.to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            }
            Err(_) => true,
        }
    }

    /// Flake root this snapshot tracks.
    #[must_use]
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Cumulative session statistics.
    #[must_use]
    pub fn stats(&self) -> &WatchSnapshotStats {
        &self.stats
    }

    /// Borrow the retained digest cache (shared across watch generations).
    #[must_use]
    pub fn digest_cache(&self) -> &RunDigestCache {
        &self.digest_cache
    }

    /// Mutable digest cache.
    pub fn digest_cache_mut(&mut self) -> &mut RunDigestCache {
        &mut self.digest_cache
    }

    /// Replace the digest cache (after partial node prepare).
    pub fn set_digest_cache(&mut self, cache: RunDigestCache) {
        self.digest_cache = cache;
    }

    /// Take ownership of the digest cache, leaving an empty cache behind.
    pub fn take_digest_cache(&mut self) -> RunDigestCache {
        std::mem::replace(&mut self.digest_cache, RunDigestCache::new())
    }

    /// Install or replace the affected graph after the first tasks snapshot loads.
    pub fn set_affected_graph_from_discovery(
        &mut self,
        apps: &[App],
        document: &TaskDocument,
        flake: &str,
        system: &str,
    ) {
        let _ = (flake, system);
        self.graph = Some(build_graph(apps, document));
    }

    /// Patch indexes for source-only filesystem changes.
    ///
    /// When `plan_node_ids` is set, returns task ids in that plan that are
    /// affected by `relative_paths` (conservative dependency propagation).
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when durable digest indexes cannot be updated.
    pub fn apply_source_changes(
        &mut self,
        relative_paths: &[String],
        plan_node_ids: Option<&[String]>,
        flake: &str,
        system: &str,
    ) -> io::Result<WatchSourcePatch> {
        if !Self::enabled() || relative_paths.is_empty() {
            return Ok(WatchSourcePatch {
                paths: Vec::new(),
                affected_plan_nodes: Vec::new(),
                action_digest_entries_removed: 0,
            });
        }

        let paths: Vec<String> = relative_paths
            .iter()
            .map(|path| normalize_relative_path(path))
            .collect();

        let removed = self
            .digest_cache
            .invalidate_source_paths(&self.root, &paths)?;

        let affected_plan_nodes = plan_node_ids
            .map(|ids| affected_plan_node_ids(self.graph.as_ref(), &paths, ids, flake, system))
            .unwrap_or_default();

        self.stats.source_patches = self.stats.source_patches.saturating_add(1);
        self.stats.paths_invalidated = self
            .stats
            .paths_invalidated
            .saturating_add(paths.len() as u64);
        self.stats.prepared_nodes_dropped = self
            .stats
            .prepared_nodes_dropped
            .saturating_add(affected_plan_nodes.len() as u64);

        record_watch_snapshot_patch();
        record_watch_paths_invalidated(paths.len() as u64);
        record_watch_prepared_nodes_dropped(affected_plan_nodes.len() as u64);

        Ok(WatchSourcePatch {
            paths,
            affected_plan_nodes,
            action_digest_entries_removed: removed,
        })
    }
}

fn affected_plan_node_ids(
    graph: Option<&AffectedGraph>,
    changed_paths: &[String],
    plan_node_ids: &[String],
    flake: &str,
    system: &str,
) -> Vec<String> {
    let Some(graph) = graph else {
        return plan_node_ids.to_vec();
    };
    let analysis = analyze(graph, changed_paths, flake, system, false);
    let affected_tasks: BTreeSet<&str> = analysis
        .nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Affected && node.kind == "task")
        .map(|node| node.name.as_str())
        .collect();
    if affected_tasks.is_empty() {
        return Vec::new();
    }
    plan_node_ids
        .iter()
        .filter(|id| affected_tasks.contains(id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use nxr_core::App;
    use nxr_task::{TaskDefinition, TaskDocument};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("lock")
    }

    #[test]
    fn apply_source_changes_invalidates_digest_memo() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/a.rs"), b"a").expect("write");
        std::fs::write(root.join("src/b.rs"), b"b").expect("write");

        let mut snapshot = WatchIncrementalSnapshot::new(root.clone());
        let first = snapshot
            .digest_cache_mut()
            .digest_repo_path(&root, "src/a.rs")
            .expect("digest a");
        assert_eq!(snapshot.digest_cache().hits(), 0);

        let patch = snapshot
            .apply_source_changes(&["src/a.rs".to_owned()], None, "./.", "aarch64-darwin")
            .expect("patch");
        assert_eq!(patch.paths, vec!["src/a.rs"]);
        assert_eq!(snapshot.stats().source_patches, 1);

        std::fs::write(root.join("src/a.rs"), b"a-changed").expect("edit");
        let second = snapshot
            .digest_cache_mut()
            .digest_repo_path(&root, "src/a.rs")
            .expect("digest a again");
        assert_ne!(first, second);
        assert_eq!(snapshot.digest_cache().hits(), 0);
    }

    #[test]
    fn affected_plan_nodes_subset() {
        let mut lint = TaskDefinition::new("lint-app");
        lint.paths = vec!["src/**".to_owned()];
        let mut test_task = TaskDefinition::new("test-app");
        test_task.paths = vec!["tests/**".to_owned()];
        let mut tasks = BTreeMap::new();
        tasks.insert("lint".to_owned(), lint);
        tasks.insert("test".to_owned(), test_task);
        let document = TaskDocument::new(tasks);
        let apps = vec![
            App {
                name: "lint-app".to_owned(),
                attr_path: "apps.lint-app".to_owned(),
                flake_ref: "./.".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "test-app".to_owned(),
                attr_path: "apps.test-app".to_owned(),
                flake_ref: "./.".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ];
        let graph = build_graph(&apps, &document);
        let mut snapshot = WatchIncrementalSnapshot::new(Utf8PathBuf::from("/proj"));
        snapshot.graph = Some(graph);

        let patch = snapshot
            .apply_source_changes(
                &["src/main.rs".to_owned()],
                Some(&["lint".to_owned(), "test".to_owned()]),
                "./.",
                "aarch64-darwin",
            )
            .expect("patch");
        assert_eq!(patch.affected_plan_nodes, vec!["lint".to_owned()]);
    }
}
