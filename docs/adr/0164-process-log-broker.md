# ADR-0164: Optional process log broker via `nxrd`

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 7c)
- **Related ADRs:** ADR-0157, ADR-0162, ADR-0130 (process MVP)

## Context

`nxr process logs --follow` opened the on-disk log and slept **200 ms** between
empty reads. That preserves process MVP semantics but adds avoidable latency
when an optional local daemon is already running for cache coordination
([ADR-0157](0157-optional-nxrd.md)). ADR-0157 reserved the `log.append` method
name so Wave 7c could extend the same JSON-lines Unix socket without a new
transport. Child-output coalescing ([ADR-0162](0162-child-output-batching.md))
explicitly deferred socket follow.

## Decision

1. **`nxrd` hosts an optional in-memory log broker** (not an execution
   authority). Streams are opaque ids; the CLI uses `{project_id}/{process}`.
2. **Methods (protocol v1, role `cache`):**
   - `log.open` — register a stream and optional on-disk path
   - `log.append` — push base64 chunks into a bounded RAM tail and notify
     subscribers (does not write the process log file)
   - `log.subscribe` — streaming response on the same connection: subscribe ack,
     then `{ "type": "chunk", "data_b64": "…" }` events, optional
     `{ "type": "eof" }`
   - `log.close` — drop stream and notify subscribers
3. **File-backed follow:** subscribe opens the log path with a dedicated FD,
   streams existing bytes when `from_start` is true (default; matches today’s
   follow), then polls the open FD every **20 ms** and pushes chunks. Process
   supervision remains the writer of the log file.
4. **Bounded retention:** at most **256 KiB** recent bytes per stream in RAM
   (`MAX_TAIL_BYTES`). Append chunks capped at **64 KiB**. At most **64**
   streams. Log bodies are the same sensitivity class as on-disk process logs —
   never a secret store; no clear secret values are retained beyond existing
   process-log policy.
5. **CLI:** `nxr process logs --follow` tries the broker when
   `NXR_LOG_BROKER` is enabled (default) and the daemon is reachable; on
   absence, kill-switch, protocol mismatch, or early broker failure it falls
   back to the 200 ms file poll. `NXR_DAEMON=off` still refuses all connects.
   Best-effort `log.open` on `nxr up` when the daemon is present.
6. **Kill-switch:** `NXR_LOG_BROKER=off` (also `0` / `false` / `no`) forces
   file follow even when `nxrd` is up.
7. **Concurrency:** the daemon accept loop spawns one thread per connection so
   long-lived subscribe streams do not stall cache RPCs.

## Validation

- Unit tests: broker append/subscribe/tail cap/kill-switch; base64 round-trip.
- Daemon socket test: `log.append` + `log.subscribe` receives chunks; file
  path follow sees appended file bytes.
- CLI: absent-daemon / `NXR_LOG_BROKER=off` preserve file-follow behavior.

## Non-goals (Wave 7c)

- Replacing on-disk process logs or making the daemon required.
- Tee’ing task DAG child pipes into the broker (may hook later at the
  ADR-0162 coalescer).
- Remote log shipping, multi-user brokers, or secret redaction beyond
  existing policy.

## Consequences

- Operators running `nxr daemon start` get lower-latency process log follow.
- Standalone `logs --follow` behavior is unchanged when the daemon is down.
- Reserved `log.append` is now implemented; experimental `eval.prepare` /
  `eval.get` / `eval.put` are Wave 8c ([ADR-0168](0168-experimental-eval-worker.md));
  `worker.register` remains `not_implemented`.
