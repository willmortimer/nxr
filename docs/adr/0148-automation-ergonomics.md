# ADR-0148: Automation ergonomics CLI surface (init/migrate/ci/selectors)

- **Status:** Proposed
- **Date:** 2026-07-27
- **Target release:** 2.8
- **Related ADRs:** ADR-0139, ADR-0142

## Context

Mise/Just users need approachable onboarding without abandoning Nix-native
leaves. The 2026-07 audit lists concrete verbs.

## Decision (authorized for 2.8 sprint)

Public CLI vocabulary additions:

- `nxr init` with templates: `rust`, `node`, `mixed`, `monorepo`
- `nxr migrate justfile` and `nxr migrate mise`
- Task selectors: `category:<name>`, `project:<name>`, `changed` (plus existing
  `app:` / `task:`)
- `nxr ci plan --json` as the provider-neutral CI plan export
- First-class report writers: JUnit, SARIF, coverage JSON, benchmark JSON
- Matrix expansion metadata for platforms / feature sets / partitions

Generated command reference derives from Clap + task schemas. A golden example
fixture repository lives under `fixtures/` (or `examples/`).

## Compatibility

Apps remain flake leaves; migrations emit NXR tasks wrapping apps, never a
second mandatory manifest.
