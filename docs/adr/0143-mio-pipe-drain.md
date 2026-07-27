# ADR-0143: Mio pipe readiness must drain until WouldBlock

- **Status:** Accepted
- **Date:** 2026-07-27
- **Target release:** 2.7.1
- **Related ADRs:** ADR-0108

## Context

Unix task supervision multiplexes child stdout/stderr with `mio`. A single
read per readiness event can leave bytes unread; Mio does not guarantee another
readiness notification until the fd is drained to `WouldBlock`. Blocking reads
after spurious readiness can stall the runner. Removing pipe registrations at
process exit can discard unread tail output.

## Decision

1. Set `O_NONBLOCK` on child stdout/stderr before poll registration.
2. On readable/read-closed events, loop reads until `WouldBlock`, EOF, or a
   fairness budget (default 1 MiB per FD per poll cycle).
3. Separate process exit from pipe lifetime: keep FDs registered until both
   streams reach EOF (or a bounded post-exit drain timeout), then emit final
   exit events only after drain completes (or document ordered
   NodeExited-then-final-chunks if events already require exit first — prefer
   drain-before-NodeExited when possible without breaking event schema).
4. Propagate unexpected poll/read errors as supervision failures; ignore only
   expected teardown kinds (`Interrupted` during shutdown, closed-source races).

## Validation

- Unit/integration tests: multi-megabyte bursts on both streams; rapid-exit
  printers; multi-fanout concurrent writers.
- No silent discard of poll errors in the task run loop.
