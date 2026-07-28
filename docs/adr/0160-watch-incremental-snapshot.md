# ADR-0160: Watch incremental workspace snapshot

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 5a)
- **Related ADRs:** ADR-0156, ADR-0157, ADR-0158, ADR-0159

## Context

Watch mode already reuses discovery snapshots and sealed prepared plans on
**source** filesystem events, but each generation still rebuilt run-scoped
digest / Merkle state from scratch. That re-walked directory inputs and
re-hashed overlapping paths even when only a small subtree changed.
ADR-0156 left `invalidate_paths` hooks; ADR-0157 retained Merkle invalidation
hints in `nxrd`; Wave 5a wires an in-process snapshot for the watch session.

## Decision

1. **`WatchIncrementalSnapshot`** (`nxr-watch`) retains one
   [`RunDigestCache`](../../crates/nxr-core/src/digest_cache.rs) (including
   `IncrementalDigestState` / `MerkleSession`) for the watch process lifetime.
2. On classified **source** changes (not metadata):
   - Normalize flake-root-relative paths.
   - Call [`RunDigestCache::invalidate_source_paths`](../../crates/nxr-core/src/digest_cache.rs)
     (path/pattern memo, Merkle `invalidate_paths`, action-digest entry drop,
     Git snapshot reset).
   - Best-effort `nxrd` `merkle.invalidate` (unchanged from Wave 4a).
   - When a tasks snapshot is loaded, use the affected graph to drop prepared
     nodes for affected plan ids only; unaffected nodes stay sealed.
   - Re-prepare dropped nodes before the next generation using the shared
     digest cache (`TaskNodePreparer::from_partial_prepared`).
3. **Metadata** changes still invalidate discovery snapshots and full plan caches
   (unchanged); the incremental snapshot is reset to empty for the flake root.
4. **Kill-switch:** `NXR_WATCH_SNAPSHOT=off` (also `0` / `false` / `no`)
   skips in-process patching (watch behaves as before Wave 5a for digest reuse).
5. **Observability:** `NXR_PERF_STATS` schema **v8** adds
   `watch_snapshot_patches`, `watch_paths_invalidated`,
   `watch_prepared_nodes_dropped`.
6. **Extension hooks (empty in 5a):** reserved fields for Wave 5b semantic
   coalesce and Wave 5c prewarm — no formatter-storm or store-exe prewarm yet.
7. **Sealed watch prepare** remains eager per ADR-0159; no live CAS‖plan
   pipeline tickets cross watch generations.

## Validation

- Unit tests: digest memo invalidation; affected plan-node subset; kill-switch.
- `invalidate_source_paths` clears path memo and recomputes after edit.
- Unaffected prepared nodes survive source patches; affected ids are re-prepared.

## Non-goals (Wave 5a)

- Semantic event coalesce (5b).
- Prewarm resolved executables / CAS handles (5c).
- Replacing FSEvents or debounce semantics.
- Required `nxrd` / in-daemon `MerkleSession` ownership (still optional).

## Consequences

- Watch sessions amortize Merkle / action-digest work across generations.
- Source edits to one subtree no longer cold-reset unrelated directory digests.
- Operators can disable with `NXR_WATCH_SNAPSHOT=off` for bisection.
