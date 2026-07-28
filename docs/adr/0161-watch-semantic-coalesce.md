# ADR-0161: Watch semantic change coalesce

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 5b)
- **Related ADRs:** ADR-0160, ADR-0112

## Context

Watch mode already coalesces filesystem noise with a temporal debounce
(ADR-0112 spirit). Wave 5a
([ADR-0160](0160-watch-incremental-snapshot.md)) added incremental digest /
Merkle patching across generations, but every debounced batch still forwarded
all pending paths to classification and invalidation.

Common editor and tooling patterns produce spurious or redundant restarts:

- atomic save via temp file + rename;
- formatter / linter storms touching many siblings;
- `flake.lock` updates bundled with unrelated paths in the same debounce window;
- task-declared workspace outputs written by the running generation (feedback loops);
- fixture-only edits in repositories that ship integration fixtures.

## Decision

1. **`WatchSemanticCoalescer`** (`nxr-watch`) runs **after** temporal debounce
   and **before** metadata/source classification. Temporal debounce remains the
   backstop.
2. The coalescer is owned by [`WatchIncrementalSnapshot`](../../crates/nxr-watch/src/snapshot.rs)
   (replacing the Wave 5a placeholder hook). When incremental snapshot is
   disabled, the CLI keeps a standalone coalescer with the same rules.
3. **Rules (conservative; classification unchanged):**
   - **Fixture-only batch:** when every path is under `fixtures/` or
     `tests/fixtures/`, drop the batch (no restart).
   - **Owned outputs:** when the active task plan declares `outputs`, drop
     changes under those paths (prefix match) to avoid feedback loops.
   - **Editor temporaries:** drop `*.tmp`, `*~`, `.#*`, `*.swp` paths when the
     batch also contains their likely final target (create→rename pairing).
   - **Lockfile batch:** when `flake.lock` is present, keep only `flake.lock`
     (metadata invalidation is already broad; siblings add no signal).
   - **Formatter burst:** when ≥3 paths share a parent directory in one batch,
     collapse them to that directory prefix for invalidation locality.
4. When coalesce drops every path in a batch, the CLI **suppresses** the
   restart and returns to the debounce wait loop (generation counter may skip).
5. **Kill-switch:** `NXR_WATCH_COALESCE=off` (also `0` / `false` / `no`)
   disables semantic rules; debounce + Wave 5a snapshot behavior remain.
6. **Wave 5c prewarm** stays a reserved hook on the snapshot; no store-exe /
   CAS-handle prewarm in 5b.

## Validation

- Unit tests: create+rename, formatter burst, lockfile batch, owned outputs,
  fixture-only, kill-switch.
- Metadata vs source classification unchanged (`flake.lock` still `Metadata`).
- Sealed / eager watch prepare unchanged ([ADR-0159](0159-cas-plan-pipeline.md)).

## Non-goals (Wave 5b)

- Cross-session or nxrd-side event buffering (optional daemon may subscribe later).
- Replacing OS notify backends or debounce windows.
- Prewarm of resolved executables / CAS handles (5c).

## Consequences

- Fewer spurious watch generations and narrower digest invalidation sets.
- Task graphs with declared `outputs` avoid self-triggered restarts when outputs
  are identifiable.
- Operators can bisect with `NXR_WATCH_COALESCE=off` without disabling Wave 5a
  snapshot patching (`NXR_WATCH_SNAPSHOT=off`).
