# Agent-first execution after Q2

**Status:** plan. Starts after NixPlane Q2.4 is `passed` and the NixPlane release rollout (`docs/qualify/RELEASE_ROLLOUT.md` in that repo) has frozen the qualified binaries. This plan follows [V4_EXECUTION_PROTOCOL.md](../vision/V4_EXECUTION_PROTOCOL.md). It does not reopen source transfer.

Nix keeps environments and store paths. NXR keeps the run. NixPlane keeps fleet desired state and the object plane. Source authority for a private commit stays on the Hub. Workers keep receiving `hub-object:<digest>`.

## What qualification already showed

Q2.2 showed a real remote build, an interrupted transfer that resumed the exact remainder, a warm archive cache, a Hub restart, and Attic publication. Q2.3 showed the Hub resolving one private commit and two physical hosts building different derivations from that archive. The Mac worker's second process reused every source byte.

Q2.4, once passed, is the first durable-run proof for this plan: one Stargate generation, a warm restore whose full `nxr` elapsed time is recorded, and one detached client that does not submit the work twice.

## Next NXR work

Do this after the release sequence. Do not start it in place of Q2.4.

1. **One event envelope.** Plan, node, reuse, cache decision, and elapsed time share one stream. An agent reads the same events the CLI prints. A reused node is a successful cache decision. It is a completed node with its output present. The summary keeps executed and reused counts separate.
2. **Durable run identity.** One attempt supports start, detach, reattach, cancel, and rerun. Q2.4 case 6 is the first proof: the detached wait exits 124 with `detached: true`, and reattach uses `NIXPLANE_REATTACH=1` on that same attempt. A second admission is a failed proof.
3. **Agent operations.** Over that protocol: list, describe, start, inspect, cancel, and fetch the failed log. Reuse stays opt-in. Interactive, migration, deploy, and state-changing tasks stay excluded.
4. **Cache-aware scheduling.** Decisions use the recorded cold and warm elapsed times from the Q2.4 receipt. Report a warm decision whose resolve and restore cost is close to the producer cost. A missing elapsed time stays `not_measured`.

## Out of scope

- A second design for source transfer, archive fetch, or private-repository credentials.
- WinNix, Vault, and SYS-5 activation.
- Moving nix-config or Lattice pins. Those move only in the NixPlane rollout, after the freeze and a healthy nixdev adoption.
- Treating a reused node as a skipped dependency or as a failure.
