//! Watch-session prewarm for likely reruns ([ADR-0163]).
//!
//! Retains resolved store executables, shell/context construction, task
//! ownership roots, and workspace-CAS metadata across source-only generations
//! so the control path stays thin while children still start fresh each run.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_affected::{AffectedGraph, roots_may_overlap_changes};
use nxr_core::EnvironmentPolicy;
use nxr_core::{
    record_watch_prewarm_cas_hit, record_watch_prewarm_cas_miss, record_watch_prewarm_context_hit,
    record_watch_prewarm_context_miss, record_watch_prewarm_ownership_shortcut,
    record_watch_prewarm_store_exe_hit, record_watch_prewarm_store_exe_miss,
};
use nxr_task::{AppliedTaskContext, WorkspaceCachePlan};

/// Kill-switch for watch prewarm (`off` / `0` / `false` / `no`).
pub const WATCH_PREWARM_ENV: &str = "NXR_WATCH_PREWARM";

/// Cumulative watch prewarm statistics (mirrored in `NXR_PERF_STATS` schema v9).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchPrewarmStats {
    pub store_exe_hits: u64,
    pub store_exe_misses: u64,
    pub context_hits: u64,
    pub context_misses: u64,
    pub cas_handle_hits: u64,
    pub cas_handle_misses: u64,
    pub ownership_shortcuts: u64,
}

/// Session-cached realised store executable for one app attr.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrewarmStoreExe {
    pub key_digest: String,
    pub program: Utf8PathBuf,
    pub arguments: Vec<String>,
}

/// Cached shell / execution-context construction for one task id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrewarmContext {
    pub context_name: Option<String>,
    pub confirm: bool,
    pub environment_policy: EnvironmentPolicy,
    pub effective_shell: Option<String>,
    pub applied_context: Option<AppliedTaskContext>,
}

/// Cached workspace-CAS metadata for one task id (no secret values).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrewarmCasHandle {
    pub action_key_digest: Option<String>,
    pub workspace_cache: Option<WorkspaceCachePlan>,
}

/// Task id → declared path roots for ownership locality ([ADR-0156]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchOwnershipIndex {
    roots: BTreeMap<String, Vec<String>>,
}

impl WatchOwnershipIndex {
    /// Build from a tasks snapshot graph (task ids match execution-plan node ids).
    #[must_use]
    pub fn from_graph(graph: &AffectedGraph) -> Self {
        let mut roots = BTreeMap::new();
        for (node_key, node) in &graph.nodes {
            if node_key.starts_with("task:") {
                roots.insert(node.name.clone(), node.path_roots.clone());
            }
        }
        Self { roots }
    }

    /// Whether any plan node might overlap `changed_paths` by declared roots.
    #[must_use]
    pub fn any_plan_node_may_overlap(
        &self,
        plan_node_ids: &[String],
        changed_paths: &[String],
    ) -> bool {
        plan_node_ids
            .iter()
            .any(|id| self.may_overlap(id, changed_paths))
    }

    /// Whether one task's declared roots may overlap `changed_paths`.
    #[must_use]
    pub fn may_overlap(&self, task_id: &str, changed_paths: &[String]) -> bool {
        match self.roots.get(task_id) {
            Some(roots) if roots.is_empty() => true,
            Some(roots) => roots_may_overlap_changes(roots, changed_paths),
            None => true,
        }
    }
}

/// In-process prewarm cache for one watch session (Wave 5c hook on the snapshot).
#[derive(Clone, Debug, Default)]
pub struct WatchPrewarm {
    store_exe: BTreeMap<String, PrewarmStoreExe>,
    context: BTreeMap<String, PrewarmContext>,
    cas_handles: BTreeMap<String, PrewarmCasHandle>,
    ownership: Option<WatchOwnershipIndex>,
    stats: WatchPrewarmStats,
}

impl WatchPrewarm {
    /// Whether watch prewarm is enabled (default on).
    #[must_use]
    pub fn enabled() -> bool {
        Self::enabled_from(std::env::var(WATCH_PREWARM_ENV).ok().as_deref())
    }

    fn enabled_from(value: Option<&str>) -> bool {
        match value {
            Some(value) => {
                let normalized = value.trim();
                if normalized.is_empty() {
                    return true;
                }
                !matches!(
                    normalized.to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            }
            None => true,
        }
    }

    /// Cumulative session statistics.
    #[must_use]
    pub fn stats(&self) -> &WatchPrewarmStats {
        &self.stats
    }

    /// Install or replace the ownership index after discovery loads.
    pub fn set_ownership_from_graph(&mut self, graph: &AffectedGraph) {
        if Self::enabled() {
            self.ownership = Some(WatchOwnershipIndex::from_graph(graph));
        }
    }

    /// Borrow the ownership index when present.
    #[must_use]
    pub fn ownership(&self) -> Option<&WatchOwnershipIndex> {
        self.ownership.as_ref()
    }

    /// Reset all prewarm entries (metadata invalidation / new snapshot).
    pub fn clear(&mut self) {
        self.store_exe.clear();
        self.context.clear();
        self.cas_handles.clear();
        self.ownership = None;
        self.stats = WatchPrewarmStats::default();
    }

    /// Look up a session-cached store executable by key digest.
    #[must_use]
    pub fn lookup_store_exe(&self, key_digest: &str) -> Option<&PrewarmStoreExe> {
        if !Self::enabled() {
            return None;
        }
        self.store_exe.get(key_digest)
    }

    /// Record a resolved store executable for reuse on the next generation.
    pub fn store_store_exe(&mut self, entry: PrewarmStoreExe) {
        if Self::enabled() {
            self.store_exe.insert(entry.key_digest.clone(), entry);
        }
    }

    /// Record one in-process store-exe hit.
    pub fn record_store_exe_hit(&mut self) {
        if Self::enabled() {
            self.stats.store_exe_hits = self.stats.store_exe_hits.saturating_add(1);
            record_watch_prewarm_store_exe_hit();
        }
    }

    /// Record one store-exe miss (disk / realise path).
    pub fn record_store_exe_miss(&mut self) {
        if Self::enabled() {
            self.stats.store_exe_misses = self.stats.store_exe_misses.saturating_add(1);
            record_watch_prewarm_store_exe_miss();
        }
    }

    /// Look up cached shell/context construction for `task_id`.
    #[must_use]
    pub fn lookup_context(&self, task_id: &str) -> Option<&PrewarmContext> {
        if !Self::enabled() {
            return None;
        }
        self.context.get(task_id)
    }

    /// Store shell/context construction for `task_id`.
    pub fn store_context(&mut self, task_id: impl Into<String>, entry: PrewarmContext) {
        if Self::enabled() {
            self.context.insert(task_id.into(), entry);
        }
    }

    /// Record one context construction hit during node reprepare.
    pub fn record_context_hit(&mut self) {
        if Self::enabled() {
            self.stats.context_hits = self.stats.context_hits.saturating_add(1);
            record_watch_prewarm_context_hit();
        }
    }

    /// Record one context construction miss.
    pub fn record_context_miss(&mut self) {
        if Self::enabled() {
            self.stats.context_misses = self.stats.context_misses.saturating_add(1);
            record_watch_prewarm_context_miss();
        }
    }

    /// Look up cached workspace-CAS metadata for `task_id`.
    #[must_use]
    pub fn lookup_cas_handle(&self, task_id: &str) -> Option<&PrewarmCasHandle> {
        if !Self::enabled() {
            return None;
        }
        self.cas_handles.get(task_id)
    }

    /// Store workspace-CAS metadata for `task_id`.
    pub fn store_cas_handle(&mut self, task_id: impl Into<String>, entry: PrewarmCasHandle) {
        if Self::enabled() {
            self.cas_handles.insert(task_id.into(), entry);
        }
    }

    /// Record one CAS metadata handle hit.
    pub fn record_cas_hit(&mut self) {
        if Self::enabled() {
            self.stats.cas_handle_hits = self.stats.cas_handle_hits.saturating_add(1);
            record_watch_prewarm_cas_hit();
        }
    }

    /// Record one CAS metadata handle miss.
    pub fn record_cas_miss(&mut self) {
        if Self::enabled() {
            self.stats.cas_handle_misses = self.stats.cas_handle_misses.saturating_add(1);
            record_watch_prewarm_cas_miss();
        }
    }

    /// Record that every plan node was skipped by ownership locality.
    pub fn record_ownership_shortcut(&mut self, count: u64) {
        if Self::enabled() && count > 0 {
            self.stats.ownership_shortcuts = self.stats.ownership_shortcuts.saturating_add(count);
            record_watch_prewarm_ownership_shortcut(count);
        }
    }

    /// Whether ownership locality can skip graph analysis for this patch.
    #[must_use]
    pub fn can_skip_affected_analysis(
        &self,
        plan_node_ids: &[String],
        changed_paths: &[String],
    ) -> bool {
        let Some(index) = self.ownership.as_ref() else {
            return false;
        };
        !index.any_plan_node_may_overlap(plan_node_ids, changed_paths)
    }
}

/// Key digest for store-exe prewarm entries (shared with disk cache).
#[must_use]
pub fn store_exe_prewarm_key(plan_attr_path: &str, app_name: &str) -> String {
    format!("{plan_attr_path}#{app_name}")
}

/// Normalize a flake-root-relative path for ownership checks.
#[must_use]
pub fn normalize_watch_path(path: &str) -> String {
    nxr_affected::normalize_relative_path(path)
}

/// Whether `path` is under `root` (flake-relative prefix match).
#[must_use]
pub fn path_under_root(path: &Utf8Path, root: &Utf8Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use nxr_core::App;
    use nxr_task::{TaskDefinition, TaskDocument};

    #[test]
    fn kill_switch_values() {
        assert!(WatchPrewarm::enabled_from(None));
        assert!(WatchPrewarm::enabled_from(Some("")));
        assert!(!WatchPrewarm::enabled_from(Some("off")));
        assert!(!WatchPrewarm::enabled_from(Some("0")));
    }

    #[test]
    fn session_store_exe_round_trip() {
        let mut prewarm = WatchPrewarm::default();
        prewarm.store_store_exe(PrewarmStoreExe {
            key_digest: "k".to_owned(),
            program: Utf8PathBuf::from("/nix/store/x"),
            arguments: vec!["run".to_owned()],
        });
        let hit = prewarm.lookup_store_exe("k").expect("hit");
        assert_eq!(hit.program.as_str(), "/nix/store/x");
    }

    #[test]
    fn ownership_index_locality() {
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
        let graph = nxr_affected::build_graph(&apps, &document);
        let index = WatchOwnershipIndex::from_graph(&graph);
        assert!(index.may_overlap("lint", &["src/a.rs".to_owned()]));
        assert!(!index.may_overlap("test", &["src/a.rs".to_owned()]));
        assert!(!index.any_plan_node_may_overlap(&["test".to_owned()], &["src/a.rs".to_owned()]));
    }

    #[test]
    fn can_skip_affected_analysis_when_no_overlap() {
        let mut prewarm = WatchPrewarm::default();
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
        let graph = nxr_affected::build_graph(&apps, &document);
        prewarm.set_ownership_from_graph(&graph);
        assert!(
            prewarm.can_skip_affected_analysis(&["test".to_owned()], &["src/main.rs".to_owned()])
        );
        assert!(!prewarm.can_skip_affected_analysis(
            &["lint".to_owned(), "test".to_owned()],
            &["src/main.rs".to_owned()]
        ));
    }
}
