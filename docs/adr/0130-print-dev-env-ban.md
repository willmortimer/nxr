# ADR-0130: Do not reconstruct shells via print-dev-env

- **Status:** Superseded
- **Date:** 2026-07-27 (index-only; full write-up never landed)
- **Target release:** 3.0
- **Superseded by:** [ADR-0171](0171-materialized-dev-environments.md)

## Context

Recorded in the ADR index and [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md) as
a guard against eagerly parsing `nix print-dev-env --json` and pretending NXR
owns interactive development-shell semantics (hooks, functions, aliases).

## Supersession

ADR-0171 keeps the **interactive-shell** prohibition, but authorizes a narrower
**process-compatible environment snapshot** for direct spawn of workspace
scripts and opted-in file-backed apps, with explicit fallback to
`nix develop -c`.

Do not revive an absolute ban; cite ADR-0171.
