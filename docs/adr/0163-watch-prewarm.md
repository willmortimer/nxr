# ADR-0163: Watch prewarm for likely reruns

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 5c)
- **Related ADRs:** ADR-0152, ADR-0153, ADR-0156, ADR-0160, ADR-0161

## Context

Watch mode reuses sealed prepared plans and incremental digest state on
source-only generations ([ADR-0160](0160-watch-incremental-snapshot.md)),
but each rerun still rebuilt parts of the control path: store-exe resolution
(disk lookup + fingerprint material), shell/context construction on affected
node reprepare, and affected-graph ownership scans. Wave 5a reserved a
`prewarm_hook` on [`WatchIncrementalSnapshot`](../../crates/nxr-watch/src/snapshot.rs)
for this work.

## Decision

1. **`WatchPrewarm`** (`nxr-watch`) replaces the Wave 5a placeholder on the
   incremental snapshot. Session-scoped entries survive source-only generations;
   metadata invalidation resets the snapshot (including prewarm).
2. **Retained across generations (in-process):**
   - **Store-exe:** resolved program + argv keyed by store-exe cache digest
     (reuses ADR-0153 disk cache on miss; in-process hit skips lookup).
   - **Shell/context:** per-task context name, environment policy, effective
     shell, and optional `AppliedTaskContext` for watch reprepare hints.
   - **Ownership index:** task id → declared path roots from the affected graph
     ([ADR-0156](0156-merkle-affected-index.md) locality); when no plan node
     can overlap a change set, skip full affected analysis.
   - **CAS metadata handles:** per-task workspace-CAS plan metadata (no secret
     values) for observability and future reuse; action keys still recompute on
     reprepare after source edits.
3. **Integration:** `resolve_app_spawn_with_prewarm`, `TaskNodePreparer` context
   hints on `from_partial_prepared`, and `task::execute_with_control` pass
   prewarm during watch task generations. App watch spawns use the same path.
4. **Kill-switch:** `NXR_WATCH_PREWARM=off` (also `0` / `false` / `no`).
5. **Observability:** `NXR_PERF_STATS` schema **v9** adds
   `watch_prewarm_store_exe_hits` / `_misses`, `watch_prewarm_context_hits` /
   `_misses`, `watch_prewarm_cas_hits` / `_misses`,
   `watch_prewarm_ownership_shortcuts`.
6. **Constraints:** sealed/eager watch prepare unchanged; no CAS‖plan pipeline
   across generations ([ADR-0159](0159-cas-plan-pipeline.md)). Children still
   start fresh each generation.

## Validation

- Unit tests: ownership locality, kill-switch, affected-analysis shortcut,
  context hints on partial reprepare.
- Store-exe prewarm hit avoids disk lookup on second generation spawn (when
  fingerprints unchanged).

## Non-goals (Wave 5c)

- Cross-session or `nxrd`-side prewarm (optional daemon may extend later).
- Prewarm across metadata-invalidating restarts.
- Lazy prep or live CAS pipeline in watch mode.

## Consequences

- Watch reruns amortise store-exe and context work on unchanged control inputs.
- Operators can bisect with `NXR_WATCH_PREWARM=off` without disabling Wave 5a
  snapshot (`NXR_WATCH_SNAPSHOT=off`) or 5b coalesce (`NXR_WATCH_COALESCE=off`).
