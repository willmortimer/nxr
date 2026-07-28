# ADR-0156: Repository Merkle / affected directory index

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 3)
- **Related ADRs:** ADR-0147, ADR-0154, ADR-0155

## Context

Directory and glob `inputs.paths` still re-walk every descendant to build
action keys even after Wave 2b per-file digests ([ADR-0155](0155-incremental-git-digests.md)).
Affected analysis walks every node × every change for ownership checks. A
directory digest should change only when a descendant changes, and long-lived
processes (future `nxrd`, watch Wave 5) need a loadable index they can
invalidate by path.

Discovery fingerprint indexes and the action-digest index stay separate
(different path sets and semantics). Store paths remain out of workspace
digests ([ADR-0147](0147-two-tier-actions.md)).

## Decision

1. **Merkle directory aggregation** (schema **v1**) under
   `…/nxr/merkle-index/<blake3(root)>.json`:
   - Leaves reuse Wave 2b action digests (`leaf_kind = action-digest-v1`) —
     Git blob–mapped or content-hashed via
     [`incremental_digest`](../../crates/nxr-core/src/incremental_digest.rs).
   - Directory digest:
     ```text
     BLAKE3(
       "nxr.merkle.dir.v1" ‖ 0x00 ‖
       for each child in sorted name order:
         name ‖ 0x00 ‖ kind('f'|'d') ‖ 0x00 ‖ child_digest
     )
     ```
   - Kill-switch: `NXR_MERKLE_INDEX=off` restores the pre-Wave-3 flat walk
     (descendant file list hashed with path-relative-to-dir + leaf digest).
2. **Session memo + invalidate:** within one process, computed directory
   digests are reused until [`invalidate_paths`](../../crates/nxr-core/src/merkle_index.rs)
   drops ancestor keys. Cold CLI processes rebuild from the filesystem (disk
   entries are not trusted until recomputed) so correctness does not depend on
   watch hooks. Durable JSON remains loadable by a future daemon.
3. **Action keys:** `RunDigestCache` / `digest_repo_path_incremental` use
   Merkle aggregation for directory inputs when enabled. Glob expansion still
   lists matching files; each file leaf uses Wave 2b digests. Directory-shaped
   literals benefit directly.
4. **Affected locality:** classify path hits with
   [`roots_may_overlap_changes`](../../crates/nxr-affected/src/paths.rs) so
   nodes whose ownership prefix cannot overlap a change skip per-path glob
   matching. Sibling directories under a shared parent stay independent.
5. **Watch hooks (comments only):** `nxr-watch` documents calling
   `invalidate_paths` before the next digest; full snapshot wiring is Wave 5.
6. **`nxr cache clear` / `status`** include the merkle-index directory.

## Validation

- Edit under `apps/foo` changes that directory digest and leaves `apps/bar`
  unchanged after `invalidate_paths` (in-session locality).
- Rename/move updates source and destination directory digests.
- `NXR_MERKLE_INDEX=off` (+ Git/action-digest kill-switches as needed) matches
  prior flat directory digests (`cas::digest_repo_path` / Wave 2b flat path).
- Bounded synthetic tree (~200 files) locality test in `merkle_index` unit
  tests / perf notes.

## Consequences

- **Action-key churn (one-time):** with Merkle on (default), directory input
  digests differ from the flat walk formula. Workspace CAS entries keyed under
  the old directory aggregation miss once — intentional; document in
  CHANGELOG / PERFORMANCE. File-only inputs are unchanged aside from Wave 2b.
- Index is optional and local; correctness prefers rebuild over stale durable
  hits on short-lived CLI invocations.
- Wave 4 (`nxrd`) should retain `MerkleSession`, call `invalidate_paths` on FS
  events, and treat the on-disk schema as the shared format.
