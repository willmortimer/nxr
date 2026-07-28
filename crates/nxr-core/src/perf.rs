//! Optional invocation counters for performance measurement (`NXR_PERF_STATS=1`).

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

/// Environment variable enabling perf counter collection and stderr emission.
pub const PERF_STATS_ENV: &str = "NXR_PERF_STATS";

static INIT: Once = Once::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Test override: 0 = follow env, 1 = force off, 2 = force on.
static FORCE: AtomicU64 = AtomicU64::new(0);

const FORCE_OFF: u64 = 1;
const FORCE_ON: u64 = 2;

static NIX_SPAWNS: AtomicU64 = AtomicU64::new(0);
static FS_METADATA: AtomicU64 = AtomicU64::new(0);
static BYTES_HASHED: AtomicU64 = AtomicU64::new(0);
static PLAN_PREPARE_US: AtomicU64 = AtomicU64::new(0);
static CAS_LOOKUP_US: AtomicU64 = AtomicU64::new(0);
static SPAWN_TO_CHILD_OUTPUT_US: AtomicU64 = AtomicU64::new(0);
static PLAN_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static PLAN_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static STORE_EXE_HITS: AtomicU64 = AtomicU64::new(0);
static STORE_EXE_MISSES: AtomicU64 = AtomicU64::new(0);
static DIGEST_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static DIGEST_METADATA_HITS: AtomicU64 = AtomicU64::new(0);
static GIT_BLOB_DIGESTS: AtomicU64 = AtomicU64::new(0);
static NODES_PREPARED: AtomicU64 = AtomicU64::new(0);
static SPAWN_PLANS_PREPARED: AtomicU64 = AtomicU64::new(0);
static SPAWN_PLANS_CANCELLED: AtomicU64 = AtomicU64::new(0);
static WATCH_SNAPSHOT_PATCHES: AtomicU64 = AtomicU64::new(0);
static WATCH_PATHS_INVALIDATED: AtomicU64 = AtomicU64::new(0);
static WATCH_PREPARED_NODES_DROPPED: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_STORE_EXE_HITS: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_STORE_EXE_MISSES: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_CONTEXT_HITS: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_CONTEXT_MISSES: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_CAS_HITS: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_CAS_MISSES: AtomicU64 = AtomicU64::new(0);
static WATCH_PREWARM_OWNERSHIP_SHORTCUTS: AtomicU64 = AtomicU64::new(0);

fn env_enabled() -> bool {
    match std::env::var(PERF_STATS_ENV) {
        Ok(value) => {
            let normalized = value.trim();
            !normalized.is_empty()
                && !matches!(
                    normalized.to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
        }
        Err(_) => false,
    }
}

fn ensure_init() {
    INIT.call_once(|| {
        if env_enabled() {
            ENABLED.store(true, Ordering::Relaxed);
        }
    });
}

/// Whether perf counters are active for this process.
#[must_use]
pub fn enabled() -> bool {
    match FORCE.load(Ordering::Relaxed) {
        FORCE_OFF => return false,
        FORCE_ON => return true,
        _ => {}
    }
    ensure_init();
    ENABLED.load(Ordering::Relaxed)
}

/// Record one Nix subprocess invocation (`run_nix` and similar).
#[inline]
pub fn record_nix_spawn() {
    if enabled() {
        NIX_SPAWNS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one filesystem metadata probe (`stat` / `metadata`).
#[inline]
pub fn record_fs_metadata() {
    if enabled() {
        FS_METADATA.fetch_add(1, Ordering::Relaxed);
    }
}

/// Accumulate bytes fed through BLAKE3 hashing on hot paths.
#[inline]
pub fn add_bytes_hashed(bytes: u64) {
    if enabled() && bytes > 0 {
        BYTES_HASHED.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Accumulate plan-prepare wall time (microseconds).
#[inline]
pub fn add_plan_prepare_us(micros: u64) {
    if enabled() && micros > 0 {
        PLAN_PREPARE_US.fetch_add(micros, Ordering::Relaxed);
    }
}

/// Accumulate workspace CAS lookup wall time (microseconds).
#[inline]
pub fn add_cas_lookup_us(micros: u64) {
    if enabled() && micros > 0 {
        CAS_LOOKUP_US.fetch_add(micros, Ordering::Relaxed);
    }
}

/// Record spawn-to-first-child-output latency (first sample per invocation wins).
#[inline]
pub fn record_spawn_to_child_output_us(micros: u64) {
    if enabled() && micros > 0 {
        let _ = SPAWN_TO_CHILD_OUTPUT_US.compare_exchange(
            0,
            micros,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// Record one prepared-plan disk cache hit.
#[inline]
pub fn record_plan_cache_hit() {
    if enabled() {
        PLAN_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one prepared-plan disk cache miss (prepare path ran).
#[inline]
pub fn record_plan_cache_miss() {
    if enabled() {
        PLAN_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one store-exe disk cache hit (direct store spawn).
#[inline]
pub fn record_store_exe_hit() {
    if enabled() {
        STORE_EXE_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one store-exe disk cache miss (realise or `nix run` fallback path).
#[inline]
pub fn record_store_exe_miss() {
    if enabled() {
        STORE_EXE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one run-scoped digest cache hit (path, walk, or pattern reuse).
#[inline]
pub fn record_digest_cache_hit() {
    if enabled() {
        DIGEST_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one metadata-gated action-digest reuse (no content re-read).
#[inline]
pub fn record_digest_metadata_hit() {
    if enabled() {
        DIGEST_METADATA_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one action digest derived from a Git blob OID (no working-tree read).
#[inline]
pub fn record_git_blob_digest() {
    if enabled() {
        GIT_BLOB_DIGESTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one task-graph node prepare (lazy or eager).
#[inline]
pub fn record_node_prepared() {
    if enabled() {
        NODES_PREPARED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one spawn-plan stage completion (schema v7+; ADR-0159).
#[inline]
pub fn record_spawn_plan_prepared() {
    if enabled() {
        SPAWN_PLANS_PREPARED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one in-flight or skipped spawn-plan cancelled on CAS hit (schema v7+).
#[inline]
pub fn record_spawn_plan_cancelled() {
    if enabled() {
        SPAWN_PLANS_CANCELLED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch incremental snapshot patch (schema v8+; ADR-0160).
#[inline]
pub fn record_watch_snapshot_patch() {
    if enabled() {
        WATCH_SNAPSHOT_PATCHES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Accumulate repo-relative paths invalidated by watch snapshot patches.
#[inline]
pub fn record_watch_paths_invalidated(count: u64) {
    if enabled() && count > 0 {
        WATCH_PATHS_INVALIDATED.fetch_add(count, Ordering::Relaxed);
    }
}

/// Accumulate prepared task nodes dropped on watch source invalidation.
#[inline]
pub fn record_watch_prepared_nodes_dropped(count: u64) {
    if enabled() && count > 0 {
        WATCH_PREPARED_NODES_DROPPED.fetch_add(count, Ordering::Relaxed);
    }
}

/// Record one in-process watch prewarm store-exe hit (schema v9+; ADR-0163).
#[inline]
pub fn record_watch_prewarm_store_exe_hit() {
    if enabled() {
        WATCH_PREWARM_STORE_EXE_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch prewarm store-exe miss (schema v9+).
#[inline]
pub fn record_watch_prewarm_store_exe_miss() {
    if enabled() {
        WATCH_PREWARM_STORE_EXE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch prewarm context construction hit (schema v9+).
#[inline]
pub fn record_watch_prewarm_context_hit() {
    if enabled() {
        WATCH_PREWARM_CONTEXT_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch prewarm context construction miss (schema v9+).
#[inline]
pub fn record_watch_prewarm_context_miss() {
    if enabled() {
        WATCH_PREWARM_CONTEXT_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch prewarm CAS metadata handle hit (schema v9+).
#[inline]
pub fn record_watch_prewarm_cas_hit() {
    if enabled() {
        WATCH_PREWARM_CAS_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one watch prewarm CAS metadata handle miss (schema v9+).
#[inline]
pub fn record_watch_prewarm_cas_miss() {
    if enabled() {
        WATCH_PREWARM_CAS_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Accumulate plan nodes skipped by watch ownership locality (schema v9+).
#[inline]
pub fn record_watch_prewarm_ownership_shortcut(count: u64) {
    if enabled() && count > 0 {
        WATCH_PREWARM_OWNERSHIP_SHORTCUTS.fetch_add(count, Ordering::Relaxed);
    }
}

/// RAII timer for plan preparation.
pub struct PlanPrepareGuard {
    started: Option<Instant>,
}

impl PlanPrepareGuard {
    /// Start timing when perf stats are enabled.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: enabled().then(Instant::now),
        }
    }
}

impl Drop for PlanPrepareGuard {
    fn drop(&mut self) {
        if let Some(started) = self.started.take() {
            add_plan_prepare_us(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
        }
    }
}

/// RAII timer for workspace CAS lookup.
pub struct CasLookupGuard {
    started: Option<Instant>,
}

impl CasLookupGuard {
    /// Start timing when perf stats are enabled.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: enabled().then(Instant::now),
        }
    }
}

impl Drop for CasLookupGuard {
    fn drop(&mut self) {
        if let Some(started) = self.started.take() {
            add_cas_lookup_us(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
        }
    }
}

/// Machine-readable snapshot emitted on process exit when enabled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PerfStats {
    pub schema_version: u32,
    pub nix_spawns: u64,
    pub fs_metadata: u64,
    pub bytes_hashed: u64,
    pub plan_prepare_us: u64,
    pub cas_lookup_us: u64,
    pub spawn_to_child_output_us: u64,
    /// Prepared-plan disk cache hits (schema v2+).
    pub plan_cache_hits: u64,
    /// Prepared-plan disk cache misses (schema v2+).
    pub plan_cache_misses: u64,
    /// Store-exe disk cache hits (schema v3+).
    pub store_exe_hits: u64,
    /// Store-exe disk cache misses (schema v3+).
    pub store_exe_misses: u64,
    /// Run-scoped digest cache hits (schema v4+).
    pub digest_cache_hits: u64,
    /// Metadata-gated action-digest reuse (schema v5+).
    pub digest_metadata_hits: u64,
    /// Digests from Git blob OID without reading working-tree bytes (schema v5+).
    pub git_blob_digests: u64,
    /// Task-graph nodes prepared this invocation (schema v6+; ADR-0158).
    pub nodes_prepared: u64,
    /// Spawn-plan stages completed (schema v7+; ADR-0159).
    pub spawn_plans_prepared: u64,
    /// Spawn-plan stages cancelled on CAS hit (schema v7+; ADR-0159).
    pub spawn_plans_cancelled: u64,
    /// Watch incremental snapshot patches (schema v8+; ADR-0160).
    pub watch_snapshot_patches: u64,
    /// Repo-relative paths invalidated by watch patches (schema v8+).
    pub watch_paths_invalidated: u64,
    /// Prepared task nodes dropped on watch source invalidation (schema v8+).
    pub watch_prepared_nodes_dropped: u64,
    /// In-process watch prewarm store-exe hits (schema v9+; ADR-0163).
    pub watch_prewarm_store_exe_hits: u64,
    /// In-process watch prewarm store-exe misses (schema v9+).
    pub watch_prewarm_store_exe_misses: u64,
    /// Watch prewarm context construction hits (schema v9+).
    pub watch_prewarm_context_hits: u64,
    /// Watch prewarm context construction misses (schema v9+).
    pub watch_prewarm_context_misses: u64,
    /// Watch prewarm CAS metadata handle hits (schema v9+).
    pub watch_prewarm_cas_hits: u64,
    /// Watch prewarm CAS metadata handle misses (schema v9+).
    pub watch_prewarm_cas_misses: u64,
    /// Plan nodes skipped by watch ownership locality (schema v9+).
    pub watch_prewarm_ownership_shortcuts: u64,
}

impl PerfStats {
    /// Schema v9 adds watch prewarm counters (ADR-0163).
    const SCHEMA_VERSION: u32 = 9;

    /// Collect current counter values.
    #[must_use]
    pub fn snapshot() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            nix_spawns: NIX_SPAWNS.load(Ordering::Relaxed),
            fs_metadata: FS_METADATA.load(Ordering::Relaxed),
            bytes_hashed: BYTES_HASHED.load(Ordering::Relaxed),
            plan_prepare_us: PLAN_PREPARE_US.load(Ordering::Relaxed),
            cas_lookup_us: CAS_LOOKUP_US.load(Ordering::Relaxed),
            spawn_to_child_output_us: SPAWN_TO_CHILD_OUTPUT_US.load(Ordering::Relaxed),
            plan_cache_hits: PLAN_CACHE_HITS.load(Ordering::Relaxed),
            plan_cache_misses: PLAN_CACHE_MISSES.load(Ordering::Relaxed),
            store_exe_hits: STORE_EXE_HITS.load(Ordering::Relaxed),
            store_exe_misses: STORE_EXE_MISSES.load(Ordering::Relaxed),
            digest_cache_hits: DIGEST_CACHE_HITS.load(Ordering::Relaxed),
            digest_metadata_hits: DIGEST_METADATA_HITS.load(Ordering::Relaxed),
            git_blob_digests: GIT_BLOB_DIGESTS.load(Ordering::Relaxed),
            nodes_prepared: NODES_PREPARED.load(Ordering::Relaxed),
            spawn_plans_prepared: SPAWN_PLANS_PREPARED.load(Ordering::Relaxed),
            spawn_plans_cancelled: SPAWN_PLANS_CANCELLED.load(Ordering::Relaxed),
            watch_snapshot_patches: WATCH_SNAPSHOT_PATCHES.load(Ordering::Relaxed),
            watch_paths_invalidated: WATCH_PATHS_INVALIDATED.load(Ordering::Relaxed),
            watch_prepared_nodes_dropped: WATCH_PREPARED_NODES_DROPPED.load(Ordering::Relaxed),
            watch_prewarm_store_exe_hits: WATCH_PREWARM_STORE_EXE_HITS.load(Ordering::Relaxed),
            watch_prewarm_store_exe_misses: WATCH_PREWARM_STORE_EXE_MISSES.load(Ordering::Relaxed),
            watch_prewarm_context_hits: WATCH_PREWARM_CONTEXT_HITS.load(Ordering::Relaxed),
            watch_prewarm_context_misses: WATCH_PREWARM_CONTEXT_MISSES.load(Ordering::Relaxed),
            watch_prewarm_cas_hits: WATCH_PREWARM_CAS_HITS.load(Ordering::Relaxed),
            watch_prewarm_cas_misses: WATCH_PREWARM_CAS_MISSES.load(Ordering::Relaxed),
            watch_prewarm_ownership_shortcuts: WATCH_PREWARM_OWNERSHIP_SHORTCUTS
                .load(Ordering::Relaxed),
        }
    }
}

/// Write a single JSON line of perf stats to stderr (diagnostics channel).
///
/// # Errors
///
/// Returns [`io::Error`] when stderr cannot be written.
pub fn emit_stderr() -> io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let stats = PerfStats::snapshot();
    let json = serde_json::to_string(&stats).map_err(io::Error::other)?;
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "nxr-perf-stats: {json}")?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_reset(enabled: bool) {
    FORCE.store(
        if enabled { FORCE_ON } else { FORCE_OFF },
        Ordering::Relaxed,
    );
    NIX_SPAWNS.store(0, Ordering::Relaxed);
    FS_METADATA.store(0, Ordering::Relaxed);
    BYTES_HASHED.store(0, Ordering::Relaxed);
    PLAN_PREPARE_US.store(0, Ordering::Relaxed);
    CAS_LOOKUP_US.store(0, Ordering::Relaxed);
    SPAWN_TO_CHILD_OUTPUT_US.store(0, Ordering::Relaxed);
    PLAN_CACHE_HITS.store(0, Ordering::Relaxed);
    PLAN_CACHE_MISSES.store(0, Ordering::Relaxed);
    STORE_EXE_HITS.store(0, Ordering::Relaxed);
    STORE_EXE_MISSES.store(0, Ordering::Relaxed);
    DIGEST_CACHE_HITS.store(0, Ordering::Relaxed);
    DIGEST_METADATA_HITS.store(0, Ordering::Relaxed);
    GIT_BLOB_DIGESTS.store(0, Ordering::Relaxed);
    NODES_PREPARED.store(0, Ordering::Relaxed);
    SPAWN_PLANS_PREPARED.store(0, Ordering::Relaxed);
    SPAWN_PLANS_CANCELLED.store(0, Ordering::Relaxed);
    WATCH_SNAPSHOT_PATCHES.store(0, Ordering::Relaxed);
    WATCH_PATHS_INVALIDATED.store(0, Ordering::Relaxed);
    WATCH_PREPARED_NODES_DROPPED.store(0, Ordering::Relaxed);
    WATCH_PREWARM_STORE_EXE_HITS.store(0, Ordering::Relaxed);
    WATCH_PREWARM_STORE_EXE_MISSES.store(0, Ordering::Relaxed);
    WATCH_PREWARM_CONTEXT_HITS.store(0, Ordering::Relaxed);
    WATCH_PREWARM_CONTEXT_MISSES.store(0, Ordering::Relaxed);
    WATCH_PREWARM_CAS_HITS.store(0, Ordering::Relaxed);
    WATCH_PREWARM_CAS_MISSES.store(0, Ordering::Relaxed);
    WATCH_PREWARM_OWNERSHIP_SHORTCUTS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_noop_when_disabled() {
        test_reset(false);
        record_nix_spawn();
        add_bytes_hashed(100);
        record_fs_metadata();
        add_plan_prepare_us(50);
        add_cas_lookup_us(25);
        record_spawn_to_child_output_us(10);
        record_plan_cache_hit();
        record_plan_cache_miss();
        record_store_exe_hit();
        record_store_exe_miss();
        record_digest_cache_hit();
        record_digest_metadata_hit();
        record_git_blob_digest();
        record_node_prepared();
        record_spawn_plan_prepared();
        record_spawn_plan_cancelled();
        let stats = PerfStats::snapshot();
        assert_eq!(stats.nix_spawns, 0);
        assert_eq!(stats.bytes_hashed, 0);
        assert_eq!(stats.fs_metadata, 0);
        assert_eq!(stats.plan_prepare_us, 0);
        assert_eq!(stats.cas_lookup_us, 0);
        assert_eq!(stats.spawn_to_child_output_us, 0);
        assert_eq!(stats.plan_cache_hits, 0);
        assert_eq!(stats.plan_cache_misses, 0);
        assert_eq!(stats.store_exe_hits, 0);
        assert_eq!(stats.store_exe_misses, 0);
        assert_eq!(stats.digest_cache_hits, 0);
        assert_eq!(stats.digest_metadata_hits, 0);
        assert_eq!(stats.git_blob_digests, 0);
        assert_eq!(stats.nodes_prepared, 0);
        assert_eq!(stats.spawn_plans_prepared, 0);
        assert_eq!(stats.spawn_plans_cancelled, 0);
    }

    #[test]
    fn counters_accumulate_when_enabled() {
        test_reset(true);
        record_nix_spawn();
        record_nix_spawn();
        add_bytes_hashed(1_024);
        record_fs_metadata();
        add_plan_prepare_us(100);
        add_cas_lookup_us(200);
        record_spawn_to_child_output_us(30);
        record_spawn_to_child_output_us(99);
        record_plan_cache_hit();
        record_plan_cache_miss();
        record_plan_cache_miss();
        record_store_exe_hit();
        record_store_exe_hit();
        record_store_exe_miss();
        record_digest_cache_hit();
        record_digest_cache_hit();
        record_digest_cache_hit();
        record_digest_metadata_hit();
        record_digest_metadata_hit();
        record_git_blob_digest();
        record_node_prepared();
        record_node_prepared();
        record_spawn_plan_prepared();
        record_spawn_plan_cancelled();
        record_spawn_plan_cancelled();
        let stats = PerfStats::snapshot();
        assert_eq!(stats.nix_spawns, 2);
        assert_eq!(stats.bytes_hashed, 1_024);
        assert_eq!(stats.fs_metadata, 1);
        assert_eq!(stats.plan_prepare_us, 100);
        assert_eq!(stats.cas_lookup_us, 200);
        assert_eq!(stats.spawn_to_child_output_us, 30);
        assert_eq!(stats.plan_cache_hits, 1);
        assert_eq!(stats.plan_cache_misses, 2);
        assert_eq!(stats.store_exe_hits, 2);
        assert_eq!(stats.store_exe_misses, 1);
        assert_eq!(stats.digest_cache_hits, 3);
        assert_eq!(stats.digest_metadata_hits, 2);
        assert_eq!(stats.git_blob_digests, 1);
        assert_eq!(stats.nodes_prepared, 2);
        assert_eq!(stats.spawn_plans_prepared, 1);
        assert_eq!(stats.spawn_plans_cancelled, 2);
        assert_eq!(stats.schema_version, 9);
    }

    #[test]
    fn plan_prepare_guard_records_elapsed() {
        test_reset(true);
        {
            let _guard = PlanPrepareGuard::start();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(PerfStats::snapshot().plan_prepare_us > 0);
    }

    #[test]
    fn emit_stderr_writes_json_line() {
        test_reset(true);
        record_nix_spawn();
        emit_stderr().expect("stderr write");
    }
}
