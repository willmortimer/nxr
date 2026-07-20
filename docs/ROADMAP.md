# Roadmap

## Roadmap principles

1. Ship a small, trustworthy runner before inventing a task language.
2. Preserve direct `nix run` compatibility at every stage.
3. Lock CLI semantics early: argument forwarding, exit codes, working directory, output streams, and signal behavior.
4. Build structured internals before rich presentation.
5. Add orchestration only after foreground app execution is excellent.
6. Treat adjacent tools as design inputs, but express the resulting workflow through Nix-native primitives.
7. Keep local development and CI on the same inspectable execution graph.
8. Defer control-plane features (daemon, CAS, remote workers, dashboards) until the 2.x line is trustworthy on real flakes.

## Shipped releases

Detailed phase write-ups through V2.0 live in git history (see tags `v1.0.0`, `v2.0.0` and earlier `docs/ROADMAP.md` revisions).

| Release | Theme | Summary |
|---|---|---|
| **V1.0** | Standard flake app runner | Discovery, execution, completion, diagnostics, doctor, `plan` — shipped as `v1.0.0`. |
| **V2.0** | Workflow orchestration | Task DAG, scheduler, supervision, watch, shell integration, structured output — shipped as `v2.0.0`. |
| **V2.1** | Trustworthiness | `WorkspaceSnapshot`, discovery cache controls, Nix forwarding, `--shell-mode`, byte-safe output, four-system CI, release SBOMs — shipped as `v2.1.0`. |
| **V2.2** | Flake UX | Standard flake output commands (`list`/`build`/`check`/`shell`), `explain` and `doctor --all`, multi-root task union DAGs, interactive-task exclusivity — shipped as `v2.2.0`. |

## Active roadmap — 2.3

The active plan is one minor release that hardens and extends what already ships. Defer speculative platform work to [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md).

### 2.3 — Monorepo ergonomics

**Goal:** Improve day-to-day use in repositories with many apps and tasks — without a second project graph or remote execution layer.

**Deliverables**

- Categories, aliases, and filtered `list` / `inspect` for large operation sets.
- Large DAG plan and summary UX (grouped/failures modes, graph formats, deterministic ordering).
- Working-directory and environment-policy conventions for nested package layouts.
- Documentation for multi-package flake layout and task naming.
- Read-only metadata adapters where they do not create a second source of truth (exploratory; no new authoritative schema). Boundary: [ADAPTERS.md](ADAPTERS.md).

**Exit criteria**

- Repositories with dozens of apps/tasks remain navigable via `list`, completion, and categories.
- Plans and graphs stay inspectable at 500+ nodes within documented time budgets.
- Monorepo guidance lives in docs; no mandatory project manifest is introduced.
- Adapters, if shipped, are optional and overrideable; leaf apps stay canonical.

## Deferred ideas

Daemon, content-addressed artifact stores, remote workers, CI control-plane generation, dashboards, and full monorepo intelligence are **not** on the active schedule. Preserved design prose lives in [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) for discussion only.

## Invariants

The following remain true for all planned 2.x work:

1. A standard flake app is always a valid leaf operation.
2. `nix run` remains a supported escape hatch.
3. Nix owns packages, runtime pinning, checks, store realizations, and native remote builds.
4. Development shells remain normal Nix outputs and integrate naturally with direnv.
5. Simple repositories do not need projects, actions, a daemon, a cache server, or workers.
6. Local and CI behavior derive from one inspectable graph.
7. Advanced metadata is versioned and additive.
8. Secrets are referenced, never embedded in store paths, plans, or public metadata.
