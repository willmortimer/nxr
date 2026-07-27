# ADR-0147: Two-tier actions — Nix store vs workspace CAS

- **Status:** Proposed
- **Date:** 2026-07-27
- **Target release:** 3.1
- **Related ADRs:** ADR-0135, ADR-0140, ADR-0209, ADR-0210

## Context

NXR should not reinvent Nix for hermetic builds, nor force mutable workspace
outputs through the store.

## Decision

**Derivation-backed actions** build `packages` / `checks` (and similar) via Nix.
Identity and artifacts live in the Nix store; binary caches and remote builders
apply. NXR never copies these into an NXR cache.

**Workspace actions** declare inputs/outputs for mutable checkout artifacts
(codegen, reports, bundles). NXR computes an action key and may restore/save
through an NXR CAS (local first; remote transport later).

Action key includes: schema major, task identity + cache salt, system,
flake.lock / relevant input identity, command + cwd, execution-context
identity, declared path digests, declared non-secret env, upstream output
digests, NXR cache protocol version.

## Validation

- Hermetic package builds never write NXR CAS entries for store paths.
- Opt-in workspace cache hit/miss explain surfaces key components.
