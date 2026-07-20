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
| **V2.3** | Monorepo ergonomics | Namespaced `list`/`inspect` views, `nxr affected` path analysis, optional read-only ecosystem graph adapters — shipped as `v2.3.0`. |
| **V2.3.1** | Trust and latency | One-process bare apps, cache v3, strict user Nix flags, affected unknown/strict, Nix-equipped release smoke — shipped as `v2.3.1`. |
| **V2.3.2** | Edge-case hardening | TTY-safe stderr, completion `discoveryInputs`, affected empty/rename/full nodes, release version/layout checks — shipped as `v2.3.2`. |

## Active roadmap

No active minor release is scheduled. **Pause feature growth** until the 2.3.x
command plane has soaked on real flakes. Speculative platform work — daemon,
CAS, remote workers, dashboards, and full monorepo intelligence — lives in
[ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) for discussion only.

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
