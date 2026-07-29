# ADR-0172: Nom-style Nix progress for interactive build ops

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** 3.4.0
- **Related ADRs:** ADR-0010
- **Supersedes:** —
- **Superseded by:** —

## Context

Direct `nix build` / `nix develop` output is dense. Operators often install
`nix-output-monitor` (`nom`) for a compact activity view. `nxr build` /
`check` / `shell` previously inherited raw Nix stderr (or tee'd it), so they
did not get that UX. Spawning `nom` unconditionally would add a hard PATH
dependency and fight CI/plain modes.

## Decision

1. Ship a **built-in** formatter that requests `nix --log-format internal-json`
   and renders a compact single-line activity status plus warning/error
   messages (CLI progress only — not a TUI).
2. Wire it into interactive `nxr build` / `nxr check` / `nxr shell` when
   stderr is a TTY (default `NXR_NIX_PROGRESS=auto`).
3. Allow `NXR_NIX_PROGRESS=off|builtin|nom`:
   - `off` — previous inherit/tee behavior;
   - `builtin` — always use the built-in formatter;
   - `nom` — if `nom` is on `PATH`, run `nom <same argv as nix>`; otherwise
     fall back to builtin.
4. Do **not** alias all Nix invocations through `nom`. App/task execution and
   captured eval paths stay unchanged.
5. Kill-switch and non-TTY/CI remain raw-compatible; JSON/`--plain` runner
   modes are unaffected.

## Validation

- Unit tests: JSON line parsing; idempotent `--log-format` injection.
- Manual: `nxr build .#nxr` on a TTY shows `…` activity; `NXR_NIX_PROGRESS=off`
  restores stock Nix stderr.

## Consequences

- Interactive build ops gain nom-like progress without requiring `nom`.
- Operators who prefer the real `nom` binary can set `NXR_NIX_PROGRESS=nom`.
