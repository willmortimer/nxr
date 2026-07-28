# ADR-0158: Staged / lazy task-graph node preparation

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 4b)
- **Related ADRs:** ADR-0010, ADR-0151, ADR-0152, ADR-0153, ADR-0157

## Context

`prepare_task_nodes` previously built spawn plans for every node in
`serial_order` before the scheduler started. That wastes Nix/plan work when:

- an upstream fails under fail-fast;
- workspace CAS hits skip spawn for a node;
- `--fail-fast` cancels never-started peers;
- affected selection already excluded branches;
- resource limits delay (or permanently starve) some ready nodes.

Wave 4c will pipeline CAS lookup with plan prep; this ADR stages preparation
so never-run nodes are not prepared, and leaves stage hooks for that pipeline.

## Decision

1. **Default to staged / lazy preparation** for live `nxr task` runs:
   - Phase 1: resolve DAG + affected roots (unchanged planner).
   - Phase 2: resource-ready scheduling (unchanged `Scheduler`).
   - Phase 3: prepare only nodes about to start (bounded by the ready set).
   - Phase 4 (optional): speculatively prepare likely successors under
     keep-going, within a budget of `--jobs`.
2. **Eager kill-switch:** `NXR_LAZY_PREP=off` (also `0` / `false` / `no`)
   restores prepare-all-up-front for bisection. Dry-run, explain, cache
   explain, and watch generation caches remain eager (they need every node
   or reuse a sealed prepared map).
3. **Correctness** for nodes that actually run is identical to eager prepare
   (same `PreparedTaskNode` fields, plan cache / store-exe reuse unchanged).
4. **Trust** is checked via context metadata scan without full prepare when
   lazy (`plan_requires_project_trust`).
5. **Observability:** `NXR_PERF_STATS` schema **v6** adds `nodes_prepared`.
6. **Wave 4c hooks:** `NodePrepStage::{CasInputs, SpawnPlan}` — fused in 4b;
   split and pipelined in ADR-0159.
7. **Daemon:** do not require `nxrd` / `eval.prepare` (ADR-0157 reserved).
   Optional hint only if trivially available later.

## Validation

- Unit tests: kill-switch parser; prepare count drops when only the first
  ready node is prepared (fail-fast / upstream failure simulation); affected
  serial subset prepares fewer nodes; speculate budget bounds successors.
- Live runs with `NXR_PERF_STATS=1` show `nodes_prepared` ≤ graph size under
  fail-fast early exit; `NXR_LAZY_PREP=off` prepares the full serial order.

## Non-goals (Wave 4b)

- Concurrent CAS lookup ‖ plan prep (Wave 4c / ADR-0159).
- Async cancel of in-flight speculative prepare (partially addressed in 4c for
  SpawnPlan tickets on CAS hit).
- Requiring nxrd or eval workers.

## Consequences

- Large DAGs that fail early or skip via CAS prepare fewer nodes by default.
- Watch still pays eager prepare when caching a generation for source-only reuse.
- Operators can force eager with `NXR_LAZY_PREP=off` if a regression is suspected.
