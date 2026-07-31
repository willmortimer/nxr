# ADR-0173: Operator TUI watch, attach, and browser

- **Status:** Accepted
- **Date:** 2026-07-31
- **Target release:** Unreleased (operator ergonomics sprint)
- **Related ADRs:** ADR-0172, ADR-0148, ADR-0011, ADR-0014

## Context

Operators want a coherent live view of task DAGs (pending / running / done /
failed), optional re-attach to a recent run, and a lazygit-style browser over
apps / tasks / scripts. Today `--output live` is a compact status line; `--log-dir`
tees files; nom covers Nix build noise. These pieces must not become three
competing UIs.

Decision branching for deploy-like flows stays **wizard flake apps → `nxr task`**,
not a second task-schema DSL.

## Decision drivers

- One renderer fed by the existing event stream.
- Preserve flake apps as leaves and `nix run` escape hatch.
- TTY-first UX with graceful non-TTY degradation for CI.
- Explicit CLI vocabulary (`tui`, `attach`, `ui`) approved for this sprint.

## Considered options

### A — Ratatui EventSink + attach + browser (chosen)

Add `ratatui`/`crossterm` in the CLI; `--output tui` consumes task events;
`nxr attach` replays/opens from history/log-dir; `nxr ui` lists and launches
with `--output tui`.

### B — Polish live line only (rejected)

Insufficient for approved ergonomics; no attach/browser.

### C — Full V4.2 protocol TUI first (deferred)

Too large; this ADR is the MVP surface that can later consume a richer run
protocol.

## Decision

1. **`--output tui`** — Ratatui one-screen DAG watch (node table + selected log
   tail). Composes with `--log-dir` tees. On non-TTY (or incompatible
   multiplex), **fall back to `--output live`** with a stderr notice (not a hard
   fail).
2. **`nxr attach [RUN]`** — Reopen the TUI for a run id from history / log-dir
   artifacts; omit RUN → most recent attachable run; fail closed if nothing
   exists.
3. **`nxr ui`** — Lazygit-style browser of apps, tasks, and workspace scripts;
   Enter runs the selection with `--output tui` (scripts via `nxr script`).
4. Events remain the sole feed for live TUI state; no parallel presentation
   protocol in this ADR.
5. Branching remains wizard apps + parameters (`--set` / TTY); no schema `when`.

## Public contract

See [CLI_CONTRACT.md](../CLI_CONTRACT.md):

- `--output tui` among output modes
- `nxr attach [RUN]`
- `nxr ui`

Kill-switches / related env (implementation):

- `NXR_OSC52=off` — disable OSC 52 failure clipboard (separate polish)
- Existing `NXR_NIX_PROGRESS` unchanged for `build`/`check`/`shell`

## Consequences

### Positive

- One watch surface operators can attach to and launch from.
- CI keeps non-interactive `live`/`grouped`/`summary` paths.

### Negative

- New GUI dependency weight (`ratatui`, `crossterm`) in `nxr-cli`.
- Alternate-screen conflict risk under some multiplexers (document; fall back).

### Neutral

- Wiki/README consumer docs land after implementation.

## Compatibility and migration

- Default `--output` behavior unchanged when `tui` is not requested.
- Bare `nxr <app>` unchanged; `ui` / `attach` are reserved commands.
- Direct `nix run` unaffected.

## Security and trust

- Sanitize untrusted names/descriptions before TUI/OSC payloads (ADR-0014).
- Never put secret values in TUI chrome, OSC 52, plans, or events.

## Validation plan

- Unit tests for TUI state machine and non-TTY fallback.
- Fixture demos for `ui` / wizard; CLI ITs without requiring a real TTY for CI.
- Manual TTY UX check for `task -j N --output tui` and `attach`.

## Rollout

Sprint `operator-ergonomics`: contract → watch+attach → `ui` → docs/GIFs.

## Unresolved questions

- Whether `attach` eventually speaks a durable run protocol (V4.1+) vs
  history/log-dir only for MVP.

## References

- [docs/PATTERNS.md](../PATTERNS.md)
- [docs/ideas/OPERATOR_ERGONOMICS.md](../ideas/OPERATOR_ERGONOMICS.md) (scratch)
- [vision/V4_EXECUTION_PROTOCOL.md](../vision/V4_EXECUTION_PROTOCOL.md) V4.2
