# Roadmap

## Roadmap principles

1. Ship a small, trustworthy runner before inventing a task language.
2. Preserve direct `nix run` compatibility at every stage.
3. Lock CLI semantics early: argument forwarding, exit codes, working directory, output streams, and signal behavior.
4. Build structured internals before rich presentation.
5. Add orchestration only after foreground app execution is excellent.
6. Treat adjacent tools as design inputs, but express the resulting workflow through Nix-native primitives.
7. Keep local development and CI on the same inspectable execution graph.
8. Expand as an **execution-context layer** for flake outputs—not a replacement for direnv, devenv, Home Manager, or secret stores (see [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md)).
9. Defer control-plane features (daemon, CAS, remote workers, dashboards) until the runner, context schema, and process model are trustworthy on real flakes.

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
| **V2.3.3** | Correctness cut | Watch ↔ task pipeline parity, empty affected = all unaffected, path validation, apps↔tasks decoupling, cache v4 BLAKE3 — shipped as `v2.3.3`. |
| **V2.4** | Run model + UX | Structured run results / `--output summary`, per-task timeouts, richer completion — shipped as `v2.4.0`; patch `v2.4.1` finishes module API, terminals, events, and shell routing. |

## Active roadmap

Design detail for everything below lives in [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).

### 2.5 — Affected execution

Keep the current plan: `task --affected` / `plan --affected`, coherent with existing
affected analysis.

### 2.6 — Ecosystem ergonomics

Low-risk, non-schema-breaking work:

- `homeManagerModules.default` (install, completion, hooks, user config; no secret values);
- `nxr fmt` (thin `nix fmt` / flake formatter wrapper);
- `nxr in <shell> <target>` (ergonomic alias of `--shell`; keep low-level flag);
- `nxr envrc` / `nxr envrc --write` (generator only; never overwrite without force);
- `nxr doctor env` and `nxr doctor cache` / `doctor builders`;
- generic `nxr build` installables / `--attr` escape hatch;
- read-only configuration / devenv / devshell adapters (`list`/`inspect`/`build`, not activate);
- shell descriptions and optional shell-entry command menu;
- treefmt / git-hooks recognition via standard flake outputs and checks.

### 3.0 — Execution-context schema

Major release: **task document schema v2**. Old runners must not silently ignore
security or execution semantics.

Schema v2 covers:

- named contexts (`perSystem.nxr.contexts`);
- `task.context` / `task.shell`;
- environment requirements;
- secret **references** + provider bindings (user/HM side);
- confirmation policy;
- structured task inputs / outputs;
- dependency states (`name@ready`, `@succeeded`, `@completed`);
- strict rejection of unknown execution-affecting fields;

Plus runtime: secret delivery (`env` / `file` / `stdin`), project trust approvals,
one-shell DAG optimization when all nodes share a context, and
`nxr context <name> …`.

### 3.1 — Process workflows

After task I/O stabilizes:

- process nodes and readiness probes;
- restart policies;
- `nxr up` / `status` / `logs`;
- task ↔ process dependency states;
- port and lifecycle metadata;

Services remain flake apps (or devenv-authored). No built-in Postgres/Redis module zoo.

### Later

Only after the above stabilize:

- artifact restoration;
- task result caching;
- remote workspace execution;
- daemon / control plane.

Speculative platform prose remains in
[ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) for discussion only.

## Invariants

The following remain true for all planned work:

1. A standard flake app is always a valid leaf operation.
2. `nix run` remains a supported escape hatch.
3. Nix owns packages, runtime pinning, checks, store realizations, and native remote builds.
4. Development shells remain normal Nix outputs and integrate naturally with direnv.
5. Simple repositories do not need projects, actions, a daemon, a cache server, or workers.
6. Local and CI behavior derive from one inspectable graph.
7. Advanced metadata is versioned; **execution/security fields must not be silently ignored**.
8. Secrets are referenced and delivered at process spawn—never embedded in store paths, plans, events, or public metadata.
9. nxr does not replace direnv, devenv, Home Manager, sops/sops-nix, or system activation tools.
