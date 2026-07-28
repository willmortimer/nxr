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

## Active roadmap

Design: [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md). ADRs: [adr/README.md](adr/README.md) (0143–0150 absorb the 2026-07 audit).

### 2.7.1 — Correctness — shipped as `v2.7.1`

1. ~~Mio: `O_NONBLOCK`, drain-until-WouldBlock, fairness budget; pipes live until EOF after exit; propagate poll errors~~ ([ADR-0143](adr/0143-mio-pipe-drain.md)).
2. ~~Auto-emit `schema_version = 2` when contexts/security fields are used~~ ([ADR-0144](adr/0144-auto-schema-v2.md)).
3. ~~Honor or hard-fail `confirm` / `context.shell` / `task.shell`~~ ([ADR-0149](adr/0149-context-shell-confirm.md)).
4. ~~Capability-cache config-file identity hashing~~ ([ADR-0145](adr/0145-capability-config-files.md)).
5. ~~Secret `provider` + `ref` distinction for env provider~~ ([ADR-0146](adr/0146-secret-provider-ref.md)).
6. ~~DeadlineQueue nearest-deadline O(log n); high-volume/rapid-exit output tests~~.
7. ~~Authoritative schema-v2 shipped/partial/planned matrix; PERFORMANCE accuracy~~.
8. ~~Tag **v2.7.1**~~.

### 2.8 — Automation ergonomics — shipped as `v2.8.0`

Mise/Just-class UX without abandoning Nix leaves ([ADR-0148](adr/0148-automation-ergonomics.md)):

- `nxr init` templates; `nxr migrate justfile|mise`
- Typed task parameters + generated completion
- Selectors (`category:`, `project:`, `changed`); matrix expansion
- Reports: JUnit, SARIF, coverage, benchmark JSON
- `nxr ci plan --json`; dogfood one canonical local/CI graph
- Generated CLI reference; golden example fixture

### 3.0 — Secure execution contexts — shipped as `v3.0.0`

Finish the security boundary before result caching:

- Complete clean/inherit/keep/set/unset; shell execution; confirmation + project trust
- Provider bindings (env, file, sops/sops-nix; Keychain/1Password/Vault as available)
- Secure tempfile + stdin delivery; audit-safe plans/events
- `nxr context list|inspect|run`; one-shell DAG optimization
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

### Later

- Optional local cache daemon deepening (watch Merkle session Wave 5; log broker
  Wave 7c shipped via [ADR-0164](adr/0164-process-log-broker.md)) — MVP daemon
  in Unreleased via [ADR-0157](adr/0157-optional-nxrd.md);
  lazy prep + CAS‖plan shipped ([ADR-0158](adr/0158-lazy-node-prep.md),
  [ADR-0159](adr/0159-cas-plan-pipeline.md)); optional `nxrMetadata` single-eval
  discovery ([ADR-0166](adr/0166-nxr-metadata-endpoint.md)); experimental opt-in
  eval worker ([ADR-0168](adr/0168-experimental-eval-worker.md)); full control
  plane remains deferred (ADR-0301 / ADR-0302)
- Remote workspace CAS transport (unblocks honest `shared` / `shared-read`);
  deterministic CI sharding; indexing daemon
- Native Nix remote builders first; GPU/capability advertisement
- Distributed workers / control plane ([ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md))
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
