# ADR-0154: Run-scoped digest deduplication for action keys

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 2a)
- **Related ADRs:** ADR-0147, ADR-0151

## Context

Workspace action keys hash repo-relative paths and globs declared in task
`inputs.paths`, plus discovery inputs and flake metadata. Task DAGs often
declare overlapping inputs (e.g. five tasks each hashing `Cargo.lock`,
`Cargo.toml`, `crates/**`), causing redundant tree walks and BLAKE3 reads
within one `nxr task` / plan / affected pass.

Persistent Merkle indexing and Git blob identity are separate waves (2b / 3).
This ADR covers **in-memory, per-invocation** memo only.

Workspace CAS store paths remain out of scope ([ADR-0147](0147-two-tier-actions.md)).

## Decision

1. Add [`RunDigestCache`](../../crates/nxr-core/src/digest_cache.rs) in
   `nxr-core`: memoize repo-relative path digests, repo file walks, and
   normalized `inputs.paths` pattern expansions for one CLI invocation.
2. Wire [`build_workspace_cache_plan`](../../crates/nxr-task/src/action_key.rs)
   and task-node preparation to share one cache per serial planning pass.
3. Correctness: action keys for identical inputs remain unchanged; cache is a
   pure optimization.
4. Extend `NXR_PERF_STATS` to schema **v4** with `digest_cache_hits` (optional
   observability).
5. Keep the API open for Wave 2b to plug Git blob identity without changing
   call sites.

## Validation

- Unit test: duplicate `digest_repo_path` calls hash file content once
  (`bytes_hashed` stable on second call).
- Integration test: two tasks with overlapping `inputs.paths` reuse digests
  (`digest_cache_hits` / internal hit counter).

## Consequences

- Warm multi-task planning does less redundant I/O; single-task runs pay only
  the cost of an empty `HashMap`.
- No on-disk state; cache is dropped when the process exits.
