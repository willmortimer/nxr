# ADR-0157: Optional local cache daemon (`nxrd`)

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 4a)
- **Related ADRs:** ADR-0010, ADR-0015, ADR-0152, ADR-0156; deferred ADR-0301 / ADR-0302

## Context

Each CLI invocation pays process start and reloads discovery / plan / Merkle
warm state from disk (or rebuilds). Watch keeps some state in-process, but
ordinary `nxr list` / `nxr plan` lose RAM caches on exit. Phase 27 of
[FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md) and deferred
ADR-0301 describe a full workspace control plane; this wave ships only a
**cache and coordination** daemon for local latency.

## Decision

1. Ship an **optional** per-user daemon reachable as `nxr daemon
   start|stop|status` (alias identity `nxrd`). Default socket:
   `$XDG_RUNTIME_DIR/nxr/nxrd.sock` (override `NXR_DAEMON_SOCKET`; fallback under
   `$TMPDIR/nxr-<user>/`).
2. Protocol: **JSON lines**, schema/protocol version **1**, role `cache`. Hello
   negotiates version; mismatch → client refuses and falls back to standalone.
3. In-memory retention (not execution authority):
   - discovery payloads (`discovery.get` / `put`)
   - prepared plans (`plan.get` / `put`; same secret-placeholder rules as
     ADR-0152 — never clear secret values)
   - capability/config fingerprint strings (`fingerprint.get` / `put`)
   - Merkle invalidation path sets (`merkle.invalidate` /
     `merkle.invalidated.get`) for Wave 5 / watch
   - recent action-key digests (`action_key.get` / `put`; digests only)
4. CLI connects when the socket is present. **Absence, `NXR_DAEMON=off` (`0` /
   `false` / `no`), or protocol mismatch must behave identically to today**
   (ADR-0301 spirit). The daemon does not replace `nix run` / spawn
   ([ADR-0010](README.md)).
5. Reserved method names (`eval.prepare`, `log.append`, `worker.register`)
   return `not_implemented` so Waves 4b / 7c / 8c can extend without a new
   transport. This ADR does **not** accept remote workers, eval workers, or a
   log broker (ADR-0302 remains deferred). Wave **7c** later implements the
   log broker methods on this socket ([ADR-0164](0164-process-log-broker.md)).
6. High-value clients first: discovery warm path in `WorkspaceSnapshot::build`,
   prepared-plan lookup/store beside the disk cache, watch best-effort
   `merkle.invalidate` on restart classification.

## Validation

- Unit tests: protocol round-trip on a temp socket; secret plan rejection;
  kill-switch parser; protocol mismatch refusal.
- CLI: `nxr daemon status` when absent; start/stop with `--socket`;
  `NXR_DAEMON=off` refuses connect; standalone `list`/`plan` unchanged when
  daemon is down.

## Non-goals (Wave 4a)

- Required install / auto-start on login.
- Making the daemon a trust anchor or execution scheduler.
- Remote workers, shared CAS transport, eval worker (8c), log broker (7c).
- Full in-process `MerkleSession` ownership inside `nxrd` (Wave 5 deepens this).

## Consequences

- Operators may run `nxr daemon start` for warmer multi-invocation sessions.
- Disk caches (discovery, plans, Merkle index) remain the durable source of
  truth for cold processes.
- Document retained surface in [PERFORMANCE.md](../PERFORMANCE.md).
