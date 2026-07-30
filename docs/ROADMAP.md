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
9. Defer control-plane features (daemon, CAS remote workers, dashboards) until the runner, context schema, and process model are trustworthy on real flakes.
10. Execution/security fields must never be silently ignored (schema v2 + auto-promotion).

## Shipped releases

Detailed phase write-ups through V2.0 live in git history (see tags `v1.0.0`, `v2.0.0` and earlier `docs/ROADMAP.md` revisions).

| Release | Theme | Summary |
|---|---|---|
| **V1.0**–**V2.5** | Runner → affected execution | See git tags `v1.0.0` … `v2.5.0`. |
| **V2.6** | Latency + ecosystem ergonomics | Capability cache, fingerprints, watch reuse, schema export, HM, `fmt`/`in`/`envrc`, doctor — `v2.6.0`. |
| **V2.7.1** | Correctness + 2.7 polish | Cap-cache layers/v4 file digests, portable archives, flake check CI, mio drain/EOF, schema v2 auto-emit, contexts, env-provider secrets, confirm/shell — `v2.7.1`. |
| **V3.0** | Secure execution contexts | Env policy, trust, secret bindings/delivery, `nxr context` — `v3.0.0`. |
| **V3.1** | Workspace actions + process MVP | Local CAS, resources, `up`/`status`/`logs`/`down`, inventory, history, coalesced discovery — `v3.1.0`–`v3.1.4`. Cache safety ([#1](https://github.com/willmortimer/nxr/issues/1), [#2](https://github.com/willmortimer/nxr/issues/2)) shipped in **v3.1.4**. |
| **V3.2** | Local orchestration performance | Plan/store-exe caches, digests/Merkle, optional `nxrd`, lazy prep, watch fast path, lean CLI, I/O batching, Determinate eval strategy, optional `nxrMetadata` — `v3.2.0`–`v3.2.1` (ADR-0151–0168). |
| **V3.5** | Operator ergonomics MVP | `--set` + TTY params, `--log-dir` + live status, release path / Cosign — `v3.5.0`. |
| **V3.3–3.4** | Workspace scripting + materialized envs | Scripts/file-backed apps, print-dev-env snapshots, one-shell DAG, params/matrix, nom-style progress — `v3.4.0` (ADR-0169–0172). |

## Active roadmap

Design: [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md). ADRs: [adr/README.md](adr/README.md)
(0143–0150 absorb the 2026-07 audit; 0169–0172 shipped in 3.4.0).
V4+ ideas: [vision/V4_EXECUTION_PROTOCOL.md](vision/V4_EXECUTION_PROTOCOL.md).

### 2.7.1 — Correctness — shipped as `v2.7.1`

1. ~~Mio: `O_NONBLOCK`, drain-until-WouldBlock, fairness budget; pipes live until EOF after exit; propagate poll errors~~ ([ADR-0143](adr/0143-mio-pipe-drain.md)).
2. ~~Auto-emit `schema_version = 2` when contexts/security fields are used~~ ([ADR-0144](adr/0144-auto-schema-v2.md)).
3. ~~Honor or hard-fail `confirm` / `context.shell` / `task.shell`~~ ([ADR-0149](adr/0149-context-shell-confirm.md)).
4. ~~Capability-cache config-file identity hashing~~ ([ADR-0145](adr/0145-capability-config-files.md)).
5. ~~Secret `provider` + `ref` distinction for env provider~~ ([ADR-0146](adr/0146-secret-provider-ref.md)).
6. ~~DeadlineQueue nearest-deadline O(log n); high-volume/rapid-exit output tests~~.
7. ~~Authoritative schema-v2 shipped/partial/planned matrix; PERFORMANCE accuracy~~.
8. ~~Tag **v2.7.1**~~.

### 2.8 — Automation ergonomics — shipped as `v2.8.0` (partial)

Mise/Just-class UX without abandoning Nix leaves ([ADR-0148](adr/0148-automation-ergonomics.md)):

- ~~`nxr init` templates; `nxr migrate justfile|mise`~~
- ~~Typed task parameters + generated completion~~ (`NXR_PARAM_*`; `__complete`
  `task-parameters` / `task-parameter-values`)
- ~~Selectors (`category:`, `project:`, `changed`)~~; ~~matrix.include expansion~~
  (`NXR_MATRIX_*`)
- Reports: JUnit + task SARIF shipped; **coverage and benchmark remain scaffold stubs**
  (empty valid documents until artifact collection exists)
- ~~`nxr ci plan --json`~~; ~~dogfood one canonical local/CI graph~~ — **shipped**
  (root flake `ci` task + GHA `nxr task ci`)
- ~~Generated CLI reference; golden example fixture~~

### 3.0 — Secure execution contexts — shipped as `v3.0.0`

Finish the security boundary before result caching:

- Complete clean/inherit/keep/set/unset; shell execution; confirmation + project trust
- Provider bindings (env, file, sops/sops-nix; Keychain/1Password/Vault as available)
- Secure tempfile + stdin delivery; audit-safe plans/events
- ~~`nxr context list|inspect|run`~~; ~~one-shell DAG optimization~~ ([ADR-0129](adr/0129-one-shell-dag.md))
- Semantic v2 validation (paths, cache policy, resources, secret slots)

### 3.1 — Workspace actions (“Nix Turborepo”) + process MVP — shipped as `v3.1.0`–`v3.1.4`

Two execution tiers ([ADR-0147](adr/0147-two-tier-actions.md)):

- **Derivation-backed:** Nix store owns identity; never NXR-cache store paths
- **Workspace actions:** declared I/O, action key, local CAS, cache explain

Plus: resource-aware scheduling / exclusivity locks; inventory CLI + coalesced
discovery ([ADR-0150](adr/0150-inventory-coalesce.md)); process MVP
(`up` / `status` / `logs` / `down`, readiness) per ADR-0132.

Correctness follow-ups through **v3.1.3**: cache-hit scheduler hang, complete
action keys, CAS atomic publish/restore, process flake/PID hardening,
flake-parts 3.1 options, `checks.*.cli-ref`.

### 3.1.4 — Workspace cache safety — shipped as `v3.1.4`

Closes trust holes before heavier CAS/context use:

1. ~~**[#1](https://github.com/willmortimer/nxr/issues/1)** — Disable workspace
   caching by default for secret-bearing tasks (env `secret = true` / context
   secrets). Optional expert override; surface reason in `nxr cache explain`.~~
2. ~~**[#2](https://github.com/willmortimer/nxr/issues/2)** — Reject
   `cache.mode` `shared-read` / `shared` until a shared transport exists.~~

### 3.2 — Local orchestration performance — shipped as `v3.2.0`

Additive caches and coordination with kill-switches (ADR-0151–0168): prepared
plan + store-exe caches; run/Git digests; Merkle affected index; optional
`nxrd`; lazy node prep + CAS‖SpawnPlan; watch snapshot/coalesce/prewarm; lean
CLI; child I/O batching + log broker; Determinate eval strategy; batched store
queries; optional `nxrMetadata`; experimental opt-in eval worker.

### 3.2.1 — Correctness polish — shipped as `v3.2.1`

Store-exe source identity; process metadata honesty (dep closure/topo, reject
unsupported restart, process context fields, readiness fail-on-timeout); Home
Manager `services.nxrd`; regression coverage.

### 3.3 — Workspace scripting — shipped in `v3.4.0`

Close the mise/Just ergonomic gap without abandoning Nix leaves
([ADR-0169](adr/0169-workspace-script-execution.md),
[ADR-0170](adr/0170-file-backed-apps.md)):

- ~~`nxr script <path|name>`; optional `.nxr/scripts/` convention; shebang execution~~
- ~~Current environment / context / confirm / `-C` policy support~~
- ~~File-backed `nxr.apps` (`file` XOR `script`) emitting standard store apps~~
- ~~Optional local live-workspace fast-path metadata; plan visibility~~
- ~~Explain / `script --list` / migrate `--scripts`/`--file-backed` / cold live
  fast path~~

Acceptance: a checked-in script runs with exact argv/streams/signals; bare
`nxr <name>` stays app-only; `nix run .#promoted` remains the escape hatch.

### 3.4 — Materialized process environments — shipped as `v3.4.0`

Accelerate shell-backed script/app runs
([ADR-0171](adr/0171-materialized-dev-environments.md); supersedes ADR-0130’s
absolute ban):

- ~~Feature-detected `nix print-dev-env` → normalized process-env snapshot + disk
  cache; optional `nxrd` `dev_env.*` retention~~
- ~~Active-shell and warm-snapshot paths with **zero** Nix subprocesses~~
- ~~Explicit process vs exact-shell semantics; unsupported constructs fall back to
  `nix develop -c`~~
- ~~Perf counters + CLI regression coverage~~
- ~~One-shell DAG optimization~~ ([ADR-0129](adr/0129-one-shell-dag.md))
- ~~`nxr cache status|gc|invalidate` deepening for discovery/plan/dev_env~~

### Later — V4+ and distributed fabric

Ordered vision: [vision/V4_EXECUTION_PROTOCOL.md](vision/V4_EXECUTION_PROTOCOL.md).
Older speculative prose: [vision/FUTURE_CONTROL_PLANE.md](vision/FUTURE_CONTROL_PLANE.md).

- V4.0 operation IR + event envelope / run protocol
- V4.1 durable runs, prompts, optional run-coordinator role for `nxrd`
- V4.2 TUI / IDE / DAP broker
- V4.3 agent + CI operational API
- V4.4 Nix-native deploy/fleet adapters (not infra reconciliation)
- V4.5 builtins.wasm + WASI/WIT operation tier
- V5 remote workspace CAS, workers, capability pools (Nix builders for
  derivations; NXR workers for mutable/interactive work)
- Determinate feature matrix beyond doctor

### Later (unchanged carry-overs)

- Remote workspace CAS transport (unblocks honest `shared` / `shared-read`);
  deterministic CI sharding; indexing daemon
- Native Nix remote builders first; GPU/capability advertisement
- Full Determinate feature matrix beyond doctor diagnostics

## Invariants

1. A standard flake app is always a valid leaf operation.
2. `nix run` remains a supported escape hatch.
3. Nix owns packages, runtime pinning, checks, store realizations, and native remote builds.
4. Development shells remain normal Nix outputs and integrate naturally with direnv.
5. Simple repositories do not need projects, actions, a daemon, a cache server, or workers.
6. Local and CI behavior derive from one inspectable graph.
7. Advanced metadata is versioned; **execution/security fields must not be silently ignored**.
8. Secrets are referenced and delivered at process spawn—never embedded in store paths, plans, events, or public metadata.
9. nxr does not replace direnv, devenv, Home Manager, sops/sops-nix, or system activation tools.
10. Nix/Determinate build and distribute reproducible artifacts; NXR owns the human command graph, affected selection, mutable workspace actions, supervision, and security contexts.
11. Bare `nxr <name>` stays app/task resolution—never an accidental local-script winner; workspace scripts use `nxr script` (or an explicit file-backed fast path).
12. Process-env snapshots accelerate operations; they do not claim interactive-shell equivalence.
