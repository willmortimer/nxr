# ADR-0167: Batched Nix store path queries

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 8b)
- **Related ADRs:** ADR-0153, ADR-0165

## Context

Store-exe validation and realisation check `/nix/store` paths for registration
and executability. ADR-0165 reserved `batched_store_queries` on
[`DiscoveryEvalPlan`](../../crates/nxr-nix/src/strategy.rs) but Wave 8a only
added eligibility flags. Per-path Nix subprocesses are unnecessary when
`nix path-info --json` can answer existence for multiple roots in one call.

## Decision

1. Implement **`store_query`** in `nxr-nix` with:
   - **`query_store_paths`** — one `nix path-info --json` for deduplicated
     `/nix/store/…` inputs; optional `references` in parsed metadata.
   - **`store_exe_paths_usable`** — batched store registration check plus
     filesystem executable probe; used by store-exe cache hits and realise.
   - **`store_path_registered`** — single-path helper with the same fallback.
2. Enable batching when [`DiscoveryEvalPlan::batched_store_queries`](../../crates/nxr-nix/src/strategy.rs)
   is true (lazy trees not explicitly disabled) and capability cache can supply
   the version banner. **`batched_store_queries_enabled_for_nix`** consults the
   capability cache without forcing refresh.
3. **Kill-switch:** `NXR_STORE_QUERIES=fs` (also `off`, `compat`, `compatibility`,
   `0`, `false`, `no`) forces filesystem-only checks — no `path-info` spawns.
4. **Fallback:** on Nix failure, invalid JSON, or disabled batching, retain
   today's `store_exe_path_usable` / `Path::exists` behavior. Store-exe miss
   paths (`nix eval` / `nix build` / `nix run`) unchanged.
5. Wire into **`resolve_app_spawn`** (CLI) and **`realise_flake_app_program`**
   (`nxr-nix`). Do not add an eval worker (Wave 8c).

## Validation

- Unit tests for JSON parsing, kill-switch aliases, and plan gating.
- Existing store-exe integration tests unchanged (`NXR_STORE_EXE_CACHE=off`
  budgets preserved).

## Non-goals (Wave 8b)

- Persistent eval worker (8c).
- Replacing `nix build` realisation when the store output is missing.
- Batching across unrelated CLI invocations (no cross-process query pool).

## Consequences

- Warm store-exe hits may spawn one `path-info` instead of relying solely on
  `metadata` when batching is enabled — trade subprocess for authoritative store
  registration on lazy-trees hosts.
- Operators can bisect with `NXR_STORE_QUERIES=fs` without disabling capability
  cache or store-exe cache.
