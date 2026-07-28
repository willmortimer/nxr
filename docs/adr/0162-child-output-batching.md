# ADR-0162: Child output event batching and terminal write coalescing

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 7a + 7b)
- **Related ADRs:** ADR-0143, ADR-0157

## Context

High-output `nxr task` DAGs multiplex many children through one `mio` poll loop
([ADR-0143](0143-mio-pipe-drain.md)). Each 32 KiB read became a separate
`StdoutChunk` / `StderrChunk` event and, in live mode, separate labeled terminal
writes. That amplified allocator, channel, and lock overhead on the hot path.
Grouped/failures modes also decoded UTF-8 on every chunk even though output is
written as raw bytes on flush.

Wave **7c** (process log broker over a Unix socket) remains deferred; this ADR
covers in-process coalescing and renderer batching only.

## Decision

1. **`ChunkCoalescer`** (`nxr-process`) merges adjacent reads for the same
   `(node, stream)` before the CLI emits task events. Default limits:
   - **64 KiB** byte budget
   - **32** complete lines (`\n`-terminated records)
   - **8 ms** latency budget (shorter than the 20 ms poll interval)
2. Coalescing runs **after** ADR-0143 drain-until-`WouldBlock`; fairness and
   EOF-after-exit semantics are unchanged. A final `flush_all` runs after the
   supervised loop drains trailing pipe data.
3. **Live output** (`output_task`) batches terminal writes through an 8 KiB
   `WriteBatch`, caps per-node pending line buffers at **256 KiB**, and writes
   UTF-8-safe chunks as raw bytes when no incremental decode is required (ANSI /
   invalid bytes still use the incremental decoder).
4. **Grouped / failures** modes store **raw bytes** in spillable buffers (no
   per-chunk UTF-8 decode); decode/sanitization stays deferred until flush.
5. **Non-goals (at 7a/7b ship):** Unix-socket log broker, `nxr process logs
   --follow` poll removal, or `nxrd` tail forwarding — delivered in Wave 7c
   ([ADR-0164](0164-process-log-broker.md)).

## Validation

- Unit tests: `ChunkCoalescer` merge/byte/line/latency/flush; rapid-exit tail
  drain in `pipe_multiplex`; coalesced chunk rendering in `output_task`.
- Correctness: existing live/grouped/failures/UTF-8/ANSI tests unchanged.
- Benchmark notes in `docs/PERFORMANCE.md` (1 GiB single task, 100×10 MiB,
  1000 short tasks — harness scenarios documented; stable thresholds TBD).

## Consequences

- Fewer events and terminal writes for bursty child output.
- Slightly higher worst-case latency (≤ 8 ms) for tiny interactive lines.
- Log broker (7c) can subscribe at the coalescer or pre-broker hook later;
  process-log follow over `nxrd` is [ADR-0164](0164-process-log-broker.md).
