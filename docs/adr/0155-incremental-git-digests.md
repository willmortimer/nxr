# ADR-0155: Incremental action digests and Git blob identity

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 2b)
- **Related ADRs:** ADR-0134, ADR-0147, ADR-0151, ADR-0154

## Context

Workspace action keys hash declared `inputs.paths` (and related material). Large
checkouts re-read file bytes on every planning pass even when filesystem
metadata already proves stability. For Git-tracked trees, clean files already
have a content identity in the index (blob OID).

Wave 2a ([ADR-0154](0154-run-digest-cache.md)) memoizes digests in-process only.
Discovery fingerprinting (ADR-0134) already
uses a metadata-gated durable index for `.nix` trees — that index must stay
separate from action-key digests (different path sets and semantics).

Store / derivation paths remain out of workspace digests
([ADR-0147](0147-two-tier-actions.md)). CAS verify/save continues to use pure
content hashing via `cas::digest_repo_path`.

## Decision

1. **Extend [`RunDigestCache`](../../crates/nxr-core/src/digest_cache.rs)** so
   `digest_repo_path` uses incremental logic ([`incremental_digest`](../../crates/nxr-core/src/incremental_digest.rs)):
   - **Clean Git-tracked file:** digest from blob OID (see mapping below).
   - **Dirty / staged / conflicted tracked file:** BLAKE3 of working-tree bytes.
   - **Untracked / non-Git:** BLAKE3 of working-tree bytes, with metadata-gated
     reuse when a durable index entry matches.
2. **Git blob → digest mapping (stable, domain-separated):**
   ```text
   digest = BLAKE3( "nxr.action-digest.git-blob.v1" ‖ 0x00 ‖ oid_hex_ascii )
   ```
   The OID is the index blob hex from a batched `git ls-files --stage -z`
   (stage 0 only). Dirty paths come from one batched
   `git status --porcelain=v1 -z`. No per-file `git` in a hot loop.
3. **Durable index** (schema **v1**) under the user cache
   `…/nxr/action-digests/<blake3(root)>.json`, separate from discovery
   fingerprint indexes. Stores device/inode/size/mtime(/ctime) + digest +
   optional `git_blob`. Kill-switch: `NXR_ACTION_DIGEST_INDEX=off`.
4. **Kill-switch:** `NXR_GIT_DIGESTS=off` forces content hashing (matches prior
   Wave 2a/`cas::digest_repo_path` behavior for inputs when the index is also
   off).
5. **Dependency choice:** shell out to `git` twice per flake root per run
   (batched). Avoids adding gitoxide/`gix` (large transitive surface) while
   meeting the no-hot-loop constraint. Revisit if Windows/Git-less hosts need a
   pure-Rust path.
6. **Perf:** `NXR_PERF_STATS` schema **v5** adds `digest_metadata_hits` and
   `git_blob_digests`; `bytes_hashed` drops when bytes are skipped.
7. **Wave 3 note:** a repo Merkle tree should consume these per-file digests
   (blob- or content-backed) as leaf values; directory aggregation today still
   walks children. Do not merge discovery fingerprint and action-digest indexes.

## Validation

- Clean tracked → digest equals domain mapping of index blob; large file does
  not inflate `bytes_hashed`.
- Dirty tracked / untracked → equals `digest_file` of working-tree bytes.
- Metadata gate: second invocation with unchanged metadata does not re-read
  content (`digest_metadata_hits`).
- `NXR_GIT_DIGESTS=off` + `NXR_ACTION_DIGEST_INDEX=off` matches
  `cas::digest_repo_path` for the same tree.
- Identical trees under the same Git/index mode produce identical action digests.

## Consequences

- Action keys for clean tracked inputs **change** vs pre-Wave-2b content BLAKE3
  when Git digests are on (default). Workspace CAS entries keyed under the old
  scheme miss once; that is intentional.
- Content-identical untracked vs clean-tracked files may disagree (blob mapping
  vs content hash) — documented; prefer tracking inputs that matter for cache.
- `nxr cache clear` / `status` include the action-digest index.
