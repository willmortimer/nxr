# ADR-0150: Generic inventory and coalesced discovery

- **Status:** Proposed
- **Date:** 2026-07-27
- **Target release:** 3.1
- **Related ADRs:** ADR-0136, ADR-0137

## Context

Inventory AST exists, but commands still project mostly standard outputs.
Cold discovery still does flake show + separate task eval.

## Decision

Add:

- `nxr inventory` / `nxr inventory --role <role>` / namespaced inspect of
  schema-described custom outputs
- `nxr build --role <role>` where roles map through exported schemas
- Optional coalesced Nix expression returning `{ inventory, nxr }` for cold
  discovery when Determinate parallel eval is available; keep separate evals
  as fallback for upstream Nix/Lix

Wasm builtins remain optional forever (never required for basic operation).

## Validation

- Fixture with custom schema role is listable via inventory.
- Coalesced path reduces Nix call count on warm/cold harness when enabled.
