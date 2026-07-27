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
| **V2.5** | Affected execution | `task --affected` / `plan --affected` wired to existing path analysis — shipped as `v2.5.0`. |
| **V2.6** | Latency + ecosystem ergonomics | Capability cache, incremental fingerprints, watch reuse, schema export, HM module, `fmt`/`in`/`envrc`, doctor env/cache/builders, installables, configuration adapters — shipped as `v2.6.0`. |

## Active roadmap

Design detail for everything below lives in [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).
Post-2.6 static re-audit (gitignored): `docs/internal/nxr-2.6-reaudit.md`.

### 2.7 — Correctness, CI contract, and warm-path polish

Ship before expanding schema-v2 runtime. Priority order from the 2.6 re-audit:

**Blockers**

1. ~~Capability-cache validity must include effective Nix configuration~~ — done
   (schema v3: binary + env layers; warm hits skip all probes when env digest matches).
2. ~~Release artifacts clearly labeled as Nix-package layouts~~ — done
   (`*-nix-package.tar.gz` + in-archive `README.txt`; portable `*-portable.tar.gz`
   per ADR-0141).
3. ~~CI dogfoods hermetic flake checks~~ — done
   (`nix flake check -L` on ubuntu/latest; other matrix cells keep app gates and
   explicit `checks.*.flake-schema`).
4. ~~CI / harness thresholds for warm-path latency and Nix-call-count regressions~~ —   done (`measure-release.sh --enforce` + `ci-thresholds.json` on ubuntu/latest;
   warm list call budgets in CLI tests).

**Warm-path polish**

5. ~~Skip rewriting an unchanged fingerprint index; incrementally cover
   `discoveryInputs`; avoid double fingerprint work in status/explain paths~~ —
   done.
6. ~~Large-monorepo / high-file-count benchmarks; qualify fingerprint “content”
   invalidation wording vs inode/size/mtime reuse~~ — done
   (`synthetic_monorepo_warm_fingerprint_scales` + `docs/PERFORMANCE.md`).
7. ~~Optional: `watch` lightweight name resolution / explicit `app:` form~~ —
   done (`app:` / `task:` prefixes; `nxr run --watch` forces apps-only snapshot).

**Also landed on `feat/2.7-correctness-ci` (critique residual polish):**

- mio/kqueue/epoll pipe multiplexing for piped task children (replaces
  thread-per-pipe on Unix).
- Expanded `doctor determinate` findings + warm capability-cache reuse
  (0 version/config probes on hit).
- Compact fingerprint index + ctime invalidation +
  `NXR_FINGERPRINT_FORCE_REHASH_SECS` (full Git fsmonitor still deferred).

**Still deferred:** generic inventory/role CLI, task-result caching,
resource-aware scheduling, Git fsmonitor fingerprint invalidation — see Later / 3.0.

### 3.0 — Execution-context schema

Major release: **task document schema v2**. Old runners must not silently ignore
security or execution semantics.

**Partial on `feat/2.7-correctness-ci` (foundations):**

- ~~Strict v2 document parse~~ (`deny_unknown_fields`; v1 unchanged).
- ~~`perSystem.nxr.contexts` module + emit~~; inspect/doctor list context names.
- ~~Env-provider secret delivery at spawn~~ (ref → caller env; plans show
  `<runtime>` only). `file` / `stdin` / sops / HM bindings still hard-error.

Still open for the 3.0 cut:

- confirmation policy / project trust approvals;
- full environment policy at runtime (inherit keep/unset completeness);
- structured task I/O runtime (cache/resources remain parse-only);
- dependency states (`name@ready`, `@succeeded`, `@completed`);
- `nxr context <name> …` CLI;
- one-shell DAG optimization when all nodes share a context;
- flake-parts default emit of `schema_version: 2` when contexts are used.

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

- shell descriptions and optional shell-entry command menu;
- treefmt / git-hooks recognition via standard flake outputs and checks;
- generic inventory / role-based custom-schema inspection (`nxr inventory` …);
- expanded `doctor determinate` (effective features, FlakeHub auth, builders);
- mio/kqueue/epoll process output multiplexing and deadline heaps;
- dogfood repository CI through one canonical NXR task DAG;
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
