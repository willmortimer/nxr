# ADR-0165: Determinate discovery/evaluation strategy planner

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 8a)
- **Related ADRs:** ADR-0150, ADR-0145

## Context

Coalesced cold discovery ([ADR-0150](0150-inventory-coalesce.md)) and Determinate
doctor probes already infer parallel eval and lazy-trees configuration, but the
CLI gated coalesced discovery inline on the version banner and only when loading
tasks. Capability-cache config probes were unused for strategy selection.

## Decision

1. Add **`plan_discovery_eval`** in `nxr-nix` (`strategy` module) returning a
   [`DiscoveryEvalPlan`](../../crates/nxr-nix/src/strategy.rs) with:
   - **`CoalescedParallelEval`** when Determinate parallel eval is available
     (or `NXR_FORCE_COALESCED_DISCOVERY` is set).
   - **`LazyTreesCompatible`** when lazy trees are enabled, or unconfigured on
     Determinate (metadata-oriented separate evals; coalesced not selected).
   - **`Compatibility`** for upstream/Lix and explicit
     `NXR_EVAL_STRATEGY=compatibility` kill-switch.
2. Cold workspace discovery (`cold_discover_workspace`) consults the plan before
   choosing coalesced vs `flake show` + separate evals; failures still fall back
   to compatibility with a stderr notice.
3. `nxr cache explain` reports `discovery_eval_strategy` alongside
   `coalesced_discovery_available`.
4. Reserve hooks (no implementation in 8a):
   - **`batched_store_queries`** → Wave 8b (`store_query` helper).
   - **`eval_worker_eligible`** → Wave 8c (always inert until worker lands).
5. Determinate Nix is never required; `nix run` escape hatch unchanged.

## Validation

- Unit tests for strategy selection across Determinate/upstream banners, lazy
  trees config, and force/kill-switch env vars.
- Existing coalesced integration test (`NXR_FORCE_COALESCED_DISCOVERY`) unchanged.
- `cache explain --json` includes `discovery_eval_strategy`.

## Non-goals (Wave 8a)

- Eval worker (8c) or batched store API (8b) beyond eligibility flags.
- `nxrMetadata` single-eval endpoint (perf-9).

## Consequences

- Strategy selection is centralized and config-aware (version banner +
  capability-cache `config_json`).
- Determinate hosts can use coalesced discovery for apps-only cold paths when
  parallel eval is available (not only `load_tasks` commands).
- Operators can bisect with `NXR_EVAL_STRATEGY=compatibility` without disabling
  capability cache.
