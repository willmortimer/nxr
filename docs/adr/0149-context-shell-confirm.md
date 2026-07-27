# ADR-0149: Context shell and confirmation must not be silently ignored

- **Status:** Accepted
- **Date:** 2026-07-27
- **Target release:** 2.7.1 (enforce), 3.0 (complete)
- **Related ADRs:** ADR-0121, ADR-0119

## Context

`confirm`, `context.shell`, and `task.shell` were parsed but ignored — unsafe
for a schema marketed as execution-context aware.

## Decision

**2.7.1:** Runtime must either apply the field or hard-fail before spawn:

- `confirm = true` → require interactive confirmation on a TTY, or
  `NXR_ASSUME_YES=1` / non-interactive explicit opt-in flag when added; otherwise
  fail closed.
- `context.shell` / `task.shell` → wrap the node via the existing `nix develop`
  shell path (same mechanism as `--shell` / `nxr in`), subject to
  `--shell-mode`. Missing shell → hard error.

**3.0:** Project trust DB, richer confirmation policy, and one-shell DAG
optimization land on this foundation.

## Validation

- Fixture `confirm = true` without assume-yes fails in non-TTY tests.
- Task with `shell` wraps via develop when shell-mode allows.
