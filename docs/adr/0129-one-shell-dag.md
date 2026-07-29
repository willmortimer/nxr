# ADR-0129: One-shell DAG optimization when all nodes share a context

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** 3.0
- **Related ADRs:** ADR-0008, ADR-0113, ADR-0128, ADR-0149, ADR-0158, ADR-0171
- **Supersedes:** —
- **Superseded by:** —

## Context

Task DAG execution today prepares each node independently. When shell-mode
requires wrapping, every node becomes:

```text
nix develop <flake>#<shell> -c nix run <flake>#<app> …
```

That repeats identical shell entry work for every node even when the resolved
shell, environment policy, and context metadata are the same across the plan.
The waste is visible on small CI-style graphs (`fmt → test → ci`) and grows
with fan-out.

[EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md) already described the preferred
shape: enter the shell once and run the scheduler inside it. ADR-0171 adds a
process-compatible alternative: materialize the shell environment once via
`nix print-dev-env` and spawn inner `nix run` children directly.

## Decision drivers

- Preserve per-node correctness for mixed-shell DAGs and distinct context env.
- Reuse ADR-0171 materialization helpers; do not invent a second snapshot format.
- Keep lazy node preparation (ADR-0158): optimization applies after nodes are
  prepared for execution, not by forcing eager prepare-all.
- Never silently merge incompatible environment policies or secret-bearing nodes.
- Workspace-action cache keys must remain stable; skip optimization when enabled
  workspace cache would observe a different argv than the optimized spawn.

## Considered options

### Option A — Always wrap per node (status quo)

Simple and correct but repeats shell entry for every node.

### Option B — Run the scheduler inside one `nix develop` (internal re-exec)

Faithful to interactive shell semantics but requires a stable internal entrypoint
and complicates stdio multiplexing, watch, and cancellation.

### Option C — Materialize once, spawn many (chosen)

When every runnable node shares the same resolved shell and compatible spawn
metadata, resolve the shell environment once (ADR-0171 `print-dev-env` on
`smart`, or a single `nix develop -c env` capture on `always`) and strip the
per-node `nix develop` wrapper from inner `nix run` argv. Mixed shells, unequal
environment policies, confirm/secret nodes, and workspace-cache-enabled nodes
keep the per-node path.

## Decision

1. **Eligibility** — apply only when:
   - at least two prepared nodes need shell wrapping (`effective_shell_wrap` is
     `Some` for the shared shell name);
   - every such node resolves to the same `plan.shell` string;
   - `plan.environment_policy`, `plan.context_env_set`, and base `environment`
     match across those nodes;
   - no node requires confirm or delivers context secrets;
   - no node has workspace-action caching enabled (would change action keys).
2. **Resolution order** (same precedence as single-node prepare):
   CLI `--shell` > `context.shell` > `task.shell`.
3. **Materialization**
   - `smart`: feature-detect `nix print-dev-env --json`; when the shell is
     process-compatible, call it once and merge the snapshot into each node's
     `EnvironmentPolicy` (`environment_mode = process`).
   - `always`, or `smart` when `print-dev-env` is unavailable/incompatible: run
     `nix develop <flake>#<shell> -c env` once, parse `KEY=VAL` lines, merge into
     each node (`environment_mode = shell`).
   - `never`, or when `NXR_DEV_SHELL` already matches (`smart` skip): no-op.
4. **Inner argv** — after materialization, replace per-node
   `develop … -c nix run …` with bare `nix run …` for wrapped nodes only.
5. **Lazy prep** — `TaskNodePreparer::try_apply_one_shell` runs metadata-only
   preflight over `serial_order` (definitions + context metadata) without preparing
   nodes. When ineligible it returns immediately; when eligible it materializes the
   shared shell once and strips per-node `nix develop` wraps lazily as each node
   reaches SpawnPlan.
6. **Observability** — dry-run / explain show the optimized inner argv and
   `environment_mode` like single-node plans.

## Validation

- Integration: shared-shell task DAG records one `print-dev-env` (`smart`) or one
  `develop` (`always`) in `NixCallCounter` while still issuing one `nix run` per
  app node.
- Integration: mixed-shell golden `ci` keeps per-node wrapping.
- Unit: shell eligibility analysis accepts matching shells and rejects mixed
  shells, secret nodes, and unlike environment policies.

## Non-goals

- Replacing direnv/devenv activation (ADR-0128).
- Reconstructing interactive shell features (hooks/functions) without fallback.
- nxrd-side shell broker or cross-invocation shell leasing.
- Changing workspace-action cache keys for optimized nodes in this release.

## Consequences

- Same-context DAG runs avoid redundant shell entry while mixed graphs behave as
  before.
- `always` mode pays one `develop` up front instead of one per node when
  `print-dev-env` is not used.
- Future workspace-cache integration may require recomputing action keys after
  optimization or tagging plans with `environment_mode`.

## References

- [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md) § one-shell optimization
- [ADR-0171](0171-materialized-dev-environments.md)
