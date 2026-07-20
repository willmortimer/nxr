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
| **V2.x bridge** | Stable extension surface | Task, execution-plan, and events schemas frozen; narrow extension points; large-graph scheduling baseline. |

## Active roadmap — 2.1 / 2.2 / 2.3

The active plan is three minor releases that harden and extend what already ships. Each section should fit on one screen; defer speculative platform work to [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md).

### 2.1 — Trustworthiness

**Goal:** Make foreground and task execution predictable on real projects and in CI.

**Deliverables**

- Cross-platform supervision gaps closed (signal escalation, orphan prevention, Windows job objects where applicable).
- Doctor and static validation expanded (impure `PATH`, missing descriptions, broken programs).
- Performance regression gates for discovery, completion, and large DAG scheduling.
- Security follow-ups from the V1 review (metadata sanitization, path handling, log redaction).
- Schema migration tooling and compatibility tests for task/plan/events V1.
- Soak and stress coverage for watcher debounce, parallel cleanup, and keep-going semantics.

**Exit criteria**

- Documented compatibility matrix matches tested platforms.
- No known orphan processes after interrupt in task runs.
- Perf baselines checked in CI; regressions fail the build.
- Breaking schema changes require a major version bump and migration note.

### 2.2 — Flake UX

**Goal:** Make standard apps and tasks feel native in daily Nix development.

**Deliverables**

- Shell integration polish: direnv-friendly hooks, nested-shell safety, completion cache tuning.
- Faster warm discovery and completion for large `apps` tables.
- Remote flake reference ergonomics and actionable evaluation errors.
- App authoring library examples and fixture flakes for common patterns.
- `inspect` / `plan` clarity for apps, tasks, shells, and stdin/argument policies.
- Migration guides from `just`, `mise`, and shell aliases (docs + small helpers where cheap).

**Exit criteria**

- `use flake` activates session-local completion without global dotfile edits.
- Completion stays responsive on warm projects; cold-start budget documented.
- Remote and local invocations show the same plan fields developers need to debug.
- New contributors can author robust apps from documented examples alone.

### 2.3 — Monorepo ergonomics

**Goal:** Improve day-to-day use in repositories with many apps and tasks — without a second project graph or remote execution layer.

**Deliverables**

- Categories, aliases, and filtered `list` / `inspect` for large operation sets.
- Large DAG plan and summary UX (grouped/failures modes, graph formats, deterministic ordering).
- Working-directory and environment-policy conventions for nested package layouts.
- Documentation for multi-package flake layout and task naming.
- Read-only metadata adapters where they do not create a second source of truth (exploratory; no new authoritative schema).

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
