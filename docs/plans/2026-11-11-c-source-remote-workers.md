# C and D — SourceSet identity, one remote operation, worker lifecycle

**Status:** design. Do not implement remote start, fleet reconcile, or a distributed DAG in this slice. The deployed v0.2.14 Hub and workers stay as they are. gmkdev stays deferred.

## C1 — SourceSet-aware operations

NXR already has `SourceSetV1` and `source_set_digest`. Extend that manifest. Do not add a second source list.

An operation may name a set of checkouts, for example Lattice, ProjectStargate, and a pinned dependency. The set records committed revisions separately from a dirty local overlay. The overlay is an identity, not an implicit `HEAD`.

A remote attempt must resolve the same source that was admitted locally. If the agent has an uncommitted change, the attempt either materializes that authorized snapshot through the existing source mechanism, or it refuses. It must not build the committed tree while the agent expects the dirty file.

Qualification: change one file in one checkout, keep that identity in the local plan, and show that a remote operation cannot run the old committed content.

## C2 — One permissioned remote operation

Cursor discovers and requests. NXR keeps the run graph, the idempotency key, the source binding, and the receipt. NixPlane Hub authenticates, checks worker eligibility, admits the attempt, and holds the lease. The enrolled NXB worker realizes, executes, publishes, and returns artifacts.

### C2.0 — Planning

A plan lists eligible targets and why each is eligible: platform, architecture, worker capabilities, required environment, source availability, authorization, and freshness. A worker whose last heartbeat is weeks old is not an available target because an old advertisement said healthy.

### C2.1 — One start

`operations_start_remote` is a separate tool. It does not reuse local-start authorization. It requires an authorized operation, a target scope, a workspace `SourceSet`, and an idempotency key.

The response carries the NXR run id and the NixPlane attempt id. Admitted is not finished.

The first proof is the qualified nixdev worker and one harmless deterministic task with an artifact that can be checked. Core-scope deployment approval does not authorize this operation. v0.2.14 changes only under a separate release.

### C2.2 — Detach, inspect, cancel

NXR owns the logical run. NixPlane owns the admitted attempt. Reconnecting inspects that attempt. It does not submit another one.

Cancel is successful only after the remote execution authority acknowledges it. Stopping the local client is not a cancel.

### C2.3 — Artifacts

Artifacts are bound to the source identity and the execution identity. Digests are checked when bytes return to the requesting workspace. A Nix store hit, an archive-cache hit, and an NXR task reuse are different events and stay labeled that way.

Qualification: one agent starts the permitted remote build, disconnects, reconnects, inspects the same attempt, retrieves a digest-verified artifact, and produces a receipt that matches those ids.

## D1 — Worker lifecycle

This follows the first remote proof and is required before agents choose among workers. Builder liveness exists on the NixPlane development branch and is not in the deployed v0.2.14 Hub.

- Protocol compatibility: Hub and worker supported version ranges, negotiated before a claim.
- Readiness: required executables, cache access, enrollment, and resources are checked before a claim.
- Liveness: fresh heartbeats and leases, not a stored healthy bit.
- Drain: no new claims while current attempts finish or expire.
- Upgrade: identify and activate one qualified worker closure.
- Health: enrollment, heartbeat, capabilities, and one real execution.
- Rollback: restore the previous qualified generation without dropping attempt authority.

gmkdev is qualified on its own when it is reachable. It is not a reason to activate other infrastructure.

## E1 — Deferred

Placing individual graph nodes on different workers waits until the single remote attempt above is qualified. NXR keeps topology, readiness, dependency binding, cache decisions, and the run result. NixPlane admits, leases, executes, and returns per-attempt results. Nix keeps derivations and store paths. The Hub does not schedule an NXR graph.
