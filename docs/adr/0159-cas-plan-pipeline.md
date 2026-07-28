# ADR-0159: CAS lookup ‖ SpawnPlan pipelining

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 4c)
- **Related ADRs:** ADR-0147, ADR-0151, ADR-0158

## Context

Wave 4b (ADR-0158) stages node preparation so never-run nodes skip work, but
CasInputs and SpawnPlan remained fused: a ready node always paid full spawn-plan
assembly before workspace CAS restore. On cache hits that wastes argv / plan
work. Wave 4c overlaps CAS lookup with SpawnPlan preparation and cancels (or
never starts) SpawnPlan on hit.

## Decision

1. **Split stages** on live lazy runs:
   - `NodePrepStage::CasInputs`: context resolution, digests, action key,
     `upstream_keys` / `RunDigestCache` updates. Command argv for the key uses
     the same assembly as `build_plan` without committing SpawnPlan.
   - `NodePrepStage::SpawnPlan`: finalize spawn via `build_plan` (program /
     arguments / plan).
2. **Pipeline (default):** after CasInputs, start SpawnPlan on a background
   thread while the scheduler performs CAS restore. Cache hit → cancel ticket
   and skip spawn. Cache miss → join ticket (or ensure SpawnPlan) then spawn.
3. **Kill-switch:** `NXR_CAS_PLAN_PIPELINE=off` (also `0` / `false` / `no`)
   fuses stages and restores serial prepare-then-CAS (Wave 4b shape).
4. **Scope:** live lazy prep only. Sealed/watch reuse and eager
   (`NXR_LAZY_PREP=off` / dry-run / explain) still prepare through SpawnPlan
   up front.
5. **Correctness:** miss-path nodes and cache-hit restores match prior
   behavior (same action keys, restore/save, spawn argv). Local CAS only.
6. **Observability:** `NXR_PERF_STATS` schema **v7** adds
   `spawn_plans_prepared` and `spawn_plans_cancelled`.
7. **Fail-fast:** never-run successors are not requested; in-flight SpawnPlan
   tickets are cancelled on CAS hit the same way fail-fast would drop them.

## Validation

- Unit tests: stage split; kill-switch fuses; hit cancels in-flight SpawnPlan;
  mixed-hit DAG prepares fewer SpawnPlans than CasInputs; fail-fast cancel;
  speculation under pipeline stops at CasInputs.
- Live runs with `NXR_PERF_STATS=1` show `spawn_plans_cancelled` on warm
  cache-hit DAGs; `NXR_CAS_PLAN_PIPELINE=off` keeps fused counts.

## Non-goals

- Remote CAS / workers.
- Moving store-exe resolve into SpawnPlan (still at spawn; Wave 5+ may deepen).
- Requiring `nxrd`.

## Consequences

- Mixed-hit DAGs prepare fewer SpawnPlans by default.
- Operators can force fused/serial with `NXR_CAS_PLAN_PIPELINE=off`.
- Watch generation caches remain eager sealed maps (no pipeline).
- Wave 5 watch paths should keep sealed/eager prepare; do not assume live
  pipeline tickets across watch generations.
