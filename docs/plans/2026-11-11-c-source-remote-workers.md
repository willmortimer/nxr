# Sprint — C1 SourceSet, then one remote build

**Status:** the sprint after B2/B3. Do not implement it in the A4 slice. v0.2.14 stays the deployed fabric. gmkdev stays deferred. Do not add a second scheduler or a new source-transfer design.

C1 and C2 both cite the versioned execution receipt from B2/B3. A remote start refuses a dirty tree that was not the admitted SourceSet snapshot. NXR keeps the graph. NixPlane admits the attempt.

## C1 — Source identity

Extend `SourceSetV1` and `source_set_digest`. Do not add another source list.

An operation names the checkouts it may read, including Lattice, ProjectStargate, and a pinned dependency. Committed revisions and a dirty local overlay are different identities. The overlay is not implicit `HEAD`.

A remote attempt must resolve the source that was admitted. If the admitted tree is dirty, the attempt materializes that snapshot through the existing source mechanism, or it refuses. It does not build the committed tree while the agent expects the dirty file.

Qualification: edit one file, keep that identity in the local plan, and show that a remote operation cannot run the old commit.

## C2 — One NixPlane operation

Cursor requests. NXR keeps the run, the idempotency key, the source binding, and the receipt. NixPlane Hub admits one attempt on the enrolled nixdev worker. The worker returns one artifact.

`operations_start_remote` is a separate tool. It does not reuse local-start authorization. The response carries the NXR run id and the NixPlane attempt id. Admitted is not finished.

The first task is harmless and deterministic. Reconnecting inspects the same attempt. Cancel counts only after NixPlane acknowledges it. The artifact digest is checked when the bytes return. A Nix store hit, an archive-cache hit, and an NXR task reuse stay labeled as different events.

Qualification: start, disconnect, reconnect, inspect the same attempt, and fetch a digest-checked artifact. Core-scope deployment approval does not authorize this operation.

## After that proof

D1 qualifies builder liveness, protocol ranges, drain, upgrade, and rollback on a later NixPlane release, not by changing v0.2.14 in place.

D2 is the multi-checkout search, affected analysis, and workflows over the SourceSet from C1.

E1 places individual graph nodes on workers only after one remote attempt is reliable. NXR keeps the graph. NixPlane admits each attempt. Nix keeps the derivations.
