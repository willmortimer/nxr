# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.5.3] - 2026-07-31

### Fixed

- Support-floor Nix **2.18** / **Lix**: parse legacy array-shaped
  `nix path-info --json` so store-exe no longer false-negatives and falls back
  to `nix run` (compat `warm_store_exe_skips_nix_run`).
- Attempt coalesced cold discovery on Compatibility hosts (not only Determinate);
  `flake show` remains the fallback. Fixes unprefixed watch flake-show budgets
  on the floor matrix.

### Changed

- Workspace and Nix package version **3.5.3**.

## [3.5.2] - 2026-07-31

### Fixed

- Stale `docs/CLI_GENERATED.md` that failed hermetic `checks.*.cli-ref` / GHA
  `nix flake check` after the 3.5.1 tag.

### Changed

- Workspace and Nix package version **3.5.2**.
- Repo quality/release path is an nxr task DAG: `ci` (host), `ci-linux` (OrbStack
  / Docker / native Linux), `release` depends on both. Prefer `nxr task …` over
  `nix run .#ci-gate` rituals; GHA `ci.yml` runs packaged `nxr task ci`.
- Host `ci` graph includes **`cli-ref`** (fail-closed on `docs/CLI_GENERATED.md`
  drift); `nix run .#cli-ref-gen` regenerates.
- Docs prefer “local ≡ CI” / self-hosted quality graph wording.

## [3.5.1] - 2026-07-30

### Fixed

- rustfmt so `fmt-check` / `nxr task ci` pass (missed before the 3.5.0 tag).
- Keyless Cosign in `release.yml` uses Cosign v3 `--bundle *.sigstore.json`
  (legacy `--output-signature` / `--output-certificate` are ignored and left
  empty paths under the new bundle format).

### Changed

- Workspace and Nix package version **3.5.1**.
- `.#release --execute` **fail-closed** unless `.nxr/release-gates/{host,linux}.<HEAD>`
  stamps exist (written by `nxr task ci` / `ci-linux`, or escape-hatch
  `.#ci-gate` / `.#ci-gate-linux`). `--skip-gates` is break-glass only.

## [3.5.0] - 2026-07-30

### Added

- Task `--set NAME=VALUE` for typed parameters (lookup: CLI → `NXR_PARAM_*` →
  default → TTY Select/Input/Confirm → fail-closed in CI).
- Global `--log-dir PATH` tees per-node stdout/stderr; live parallel runs show a
  compact node status line on stderr TTYs.
- `nix run .#release` / `nxr task release` — version sync + clean-tree checks,
  prints (or `--execute`s) `git tag -s` / push for the Cargo.toml SemVer.
- OrbStack Linux CI gate: `nix run .#ci-gate-linux` (machine `nxr-ci-linux`).
- Keyless Cosign signatures on GitHub Release blobs (`.sig` / `.pem`).
- Operator patterns: [docs/PATTERNS.md](docs/PATTERNS.md) (decisions, watch,
  promote boundaries). Vision docs live under `docs/vision/`; `docs/ideas/` is
  gitignored scratch (like `docs/internal/`).

### Fixed

- Linux process supervision: `/proc/<pid>/stat` starttime is field index 19, not
  20 (`vsize`). The off-by-one made live processes look recycled → `stopped`
  after `nxr up` on Linux only.
- `nxr script` / live file-backed apps no longer cold-eval the task document
  unless `--context` is set, restoring the zero-Nix lean path for plain scripts.
- `fixtures/configurations` evaluates on Linux `nix flake show` (set rootfs /
  disable grub / `stateVersion`) so configuration adapter tests pass in CI.
- Watch name-collision CLI test uses per-stage timeouts so parallel nextest load
  does not starve later DAG nodes after a slow first `nix run`.
- Fixture `git commit` helpers isolate from host signing agents (e.g. 1Password).

### Changed

- Workspace and Nix package version **3.5.0**.
- CI: main/PR quality is a single `ubuntu-latest` job; Nix 2.18, Lix, and macOS
  matrix moved to tag-triggered `compat.yml`.
- GHA pins Determinate Nix via `determinate-nix-action@v3.21.8` (compat `2.18`
  cells keep `nix-installer-action@v22` + `nix-package-url`).
- Local ≡ CI via `nix run .#ci-gate` (host) and `nix run .#ci-gate-linux`
  (OrbStack); quality apps clear `NXR_DEV_SHELL` and isolate git config;
  `configurations-fixture` nix check force-evals Linux NixOS assertions on Darwin
  too.

## [3.4.0] - 2026-07-28

Workspace scripting, materialized process environments, leftover 2.8 ergonomics,
and audit/HM polish in one minor.

### Added

- `nxr script <path|name>` — run a local workspace script (exact path or
  `.nxr/scripts/<name>`) through the normal process model without a flake app
  leaf ([ADR-0169](docs/adr/0169-workspace-script-execution.md)).
- File-backed `nxr.apps` (`file` XOR `script`) with optional `fastPath` live
  workspace execution metadata ([ADR-0170](docs/adr/0170-file-backed-apps.md)).
- `nxr script --list`; migrate `--scripts` / `--file-backed`; cold live fast-path
  listing eval; explain reports fast-path selection.
- Materialized process environments via feature-detected `nix print-dev-env`
  ([ADR-0171](docs/adr/0171-materialized-dev-environments.md)): opt-in disk +
  optional `nxrd` `dev_env.*` cache (`NXR_DEV_ENV_CACHE=on`); warm 0-Nix spawn;
  `--shell-mode always` develop escape.
- One-shell DAG optimization for shared shell contexts
  ([ADR-0129](docs/adr/0129-one-shell-dag.md)).
- Schema v2 typed task `parameters` (`NXR_PARAM_*`) and `matrix.include`
  (`NXR_MATRIX_*`); `__complete task-parameters` / `task-parameter-values`.
- `nxr cache status|gc|invalidate` deepening for discovery, plan, and dev-env
  caches (disk ages + nxrd warm-layer counts).
- Root flake `ci` task graph; CI runs `nxr ci plan --json` + `nxr task ci`.
- Fixture `fixtures/workspace-scripts` + CLI coverage for script argv, convention
  names, bare-app non-collision, zero-Nix no-shell path, and live fast path.
- Nom-style Nix progress for interactive `nxr build` / `check` / `shell`
  (`NXR_NIX_PROGRESS=auto|builtin|nom|off`; [ADR-0172](docs/adr/0172-nix-progress-formatter.md)).

### Changed

- Workspace and Nix package version **3.4.0**.
- Roadmap: **3.3** / **3.4** and leftover 2.8 params/matrix/CI graph marked
  shipped; ADR-0130 superseded by ADR-0171.
- Product positioning: flake-native orchestration schema that preserves standard
  Nix leaves (README / CONTRACT_SUMMARY)—not an absolute “never a task DSL.”
- V4+ vision framed in [docs/vision/V4_EXECUTION_PROTOCOL.md](docs/vision/V4_EXECUTION_PROTOCOL.md);
  legacy control-plane prose remains ideas-only.
- PERFORMANCE: measured `services.nxrd` recommendation after local warm/cold
  checks (enable; keep `evalWorker` off).

### Fixed

- `task-document.nix` tolerates missing `parameters` / `runtimeInputs` (and related
  attrs) so `nix flake show` and cold discovery evaluate against partial smoke
  fixtures.
- Prefer `nix build .#packages.$system.default` when `flake.nxr` task metadata is
  present (installable `.#nxr` can resolve to the document, not the package).
- Home Manager module uses `programs.zsh.initContent` when available (HM ≥26.05),
  with `initExtra` fallback for older Home Manager.
- Dev-environment disk/daemon retention sanitizes known secret names, rejects
  poisoned `variables` maps, and writes `0700`/`0600` entries; persistence stays
  opt-in via `NXR_DEV_ENV_CACHE=on`.
- Workspace action keys digest resolved parameter and matrix values; required
  params fail at plan/CasInputs before cache restore.
- One-shell DAG metadata preflight avoids eagerly preparing ineligible graphs.
- Task store-executable reuse preserves preparation-time Nix flags/version.
- Root `ci` uses no-op `ok` leaf; cache invalidate rejects path-traversal stems;
  parameter/matrix names are canonicalized; ambiguous `.nxr/scripts` names and
  `env -S` shebangs are handled explicitly.
- `nxr script --context` / `nxr context run … script` share trust/confirm/secrets;
  live file-backed apps apply listing `runtime_path` from `runtimeInputs`.
- Home Manager `services.nxrd` launchd agent: crash-only KeepAlive + throttle
  (so clean `nxr daemon stop` is not fought), plus stdout/stderr log paths.

## [3.2.1] - 2026-07-28

Correctness polish on the 3.2 performance surface: store-exe source identity,
process metadata honesty, Home Manager `nxrd`, and regression coverage.

### Fixed

- Store-exe / prepared-plan fingerprints now include git source identity and real
  `discoveryInputs` (schema v2). Dirty trees without declared discovery inputs
  refuse store-exe reuse so package `src/` edits cannot spawn a stale
  `/nix/store` program ([ADR-0153](docs/adr/0153-store-exe-cache.md)).
- Process `up` expands `dependsOn` closure and starts in topological order;
  rejects `restart` other than `never`; fails when readiness times out or the
  child exits; rejects `https://` readiness URLs; supports process `context`,
  `workingDirectory`, `arguments`, and `shell`.

### Added

- Home Manager `services.nxrd.enable` (systemd-user / launchd) for long-lived
  `nxrd`; `nxr envrc` documents that direnv must not start the daemon.
- Package-backed `fixtures/package-app` + store-exe source-edit regression test.

### Changed

- ROADMAP 2.8: typed parameters / matrix expansion marked planned; coverage and
  benchmark reports labeled scaffold stubs.
- Workspace and Nix package version **3.2.1**.

## [3.2.0] - 2026-07-28

Local orchestration performance: caches, digests/Merkle, optional `nxrd`, watch
fast path, lean CLI, I/O batching, and Determinate-aware discovery — all
additive with kill-switches; `nix run` escape hatch preserved.

### Added

- Optional performance counters via `NXR_PERF_STATS=1` (JSON line on stderr at exit; [ADR-0151](docs/adr/0151-perf-counters.md)).
- Extended perf harness [`scripts/perf/measure-matrix.sh`](scripts/perf/measure-matrix.sh) for task DAG, affected, and fingerprint scenarios.
- Optional prepared app-plan disk cache (schema v1; `NXR_PLAN_CACHE=off` kill-switch; [ADR-0152](docs/adr/0152-prepared-plan-cache.md)). Warm `nxr plan` / prepare reuses argv when fingerprints match; secrets are never stored. `nxr cache clear`/`status` include the plans cache. `NXR_PERF_STATS` schema **v2** adds `plan_cache_hits` / `plan_cache_misses`.
- Optional realised store-executable cache (schema v1; `NXR_STORE_EXE_CACHE=off` kill-switch; [ADR-0153](docs/adr/0153-store-exe-cache.md)). Warm app/task/process leaves can spawn the cached `/nix/store` program with 0× `nix run` when fingerprints match; miss falls back to `nix run`. `nxr cache clear`/`status` include store-exe. `NXR_PERF_STATS` schema **v3** adds `store_exe_hits` / `store_exe_misses`.
- Run-scoped digest deduplication for workspace action keys ([ADR-0154](docs/adr/0154-run-digest-cache.md)). Overlapping task `inputs.paths` share BLAKE3 results within one invocation. `NXR_PERF_STATS` schema **v4** adds `digest_cache_hits`.
- Incremental action digests with Git blob identity for clean tracked inputs ([ADR-0155](docs/adr/0155-incremental-git-digests.md)). Metadata-gated durable index (`NXR_ACTION_DIGEST_INDEX=off` kill-switch); `NXR_GIT_DIGESTS=off` forces content hashing. `NXR_PERF_STATS` schema **v5** adds `digest_metadata_hits` / `git_blob_digests`. `nxr cache clear`/`status` include the action-digest index.
- Repository Merkle / directory digest index ([ADR-0156](docs/adr/0156-merkle-affected-index.md)). Directory `inputs.paths` aggregate child digests (`NXR_MERKLE_INDEX=off` restores the flat walk). One-time action-key churn for directory-shaped inputs when Merkle is on. Affected analysis uses path-prefix locality; `nxr cache clear`/`status` include the merkle index.
- Optional local cache/coordination daemon (`nxr daemon` / `nxrd`; [ADR-0157](docs/adr/0157-optional-nxrd.md)). Unix-socket JSON-lines protocol v1 retains warm discovery, prepared plans, fingerprints, Merkle invalidation hints, and action-key digests across invocations. CLI falls back when absent; `NXR_DAEMON=off` refuses connect. Not an execution authority (ADR-0301 spirit).
- Staged / lazy task-graph node preparation ([ADR-0158](docs/adr/0158-lazy-node-prep.md)). Live `nxr task` prepares only nodes approaching execution; fail-fast / upstream failure / affected exclusion skip never-run prepares. `NXR_LAZY_PREP=off` restores eager prepare-all. `NXR_PERF_STATS` schema **v6** adds `nodes_prepared`. Wave 4c hooks (`NodePrepStage`) leave room for CAS‖plan pipelining.
- CAS lookup ‖ SpawnPlan pipelining ([ADR-0159](docs/adr/0159-cas-plan-pipeline.md)). Live lazy runs complete CasInputs (action key + digests) without finalized spawn argv; CAS restore overlaps SpawnPlan and cancels on hit. `NXR_CAS_PLAN_PIPELINE=off` fuses stages. `NXR_PERF_STATS` schema **v7** adds `spawn_plans_prepared` / `spawn_plans_cancelled`.
- Lean CLI startup + shell-resident fast path (perf Wave 6). `nxr --version`, `nxr completion`, and warm `nxr __complete` avoid Nix probes; completion scripts add flake-root lookup and optional `nxrd` socket forwarding via `_nxr_invoke` / `__nxr_invoke`. See [PERFORMANCE.md](docs/PERFORMANCE.md#lean-cli-startup--shell-fast-path-wave-6).
- Watch incremental workspace snapshot ([ADR-0160](docs/adr/0160-watch-incremental-snapshot.md)). Source-only generations patch in-process digest / Merkle state and drop only affected prepared task nodes. `NXR_WATCH_SNAPSHOT=off` kill-switch. `NXR_PERF_STATS` schema **v8** adds watch snapshot counters.
- Watch semantic change coalesce ([ADR-0161](docs/adr/0161-watch-semantic-coalesce.md)). After debounce, drop editor temporaries, collapse formatter bursts, narrow lockfile batches, and ignore fixture-only / task-owned output paths. `NXR_WATCH_COALESCE=off` kill-switch.
- Watch prewarm for likely reruns ([ADR-0163](docs/adr/0163-watch-prewarm.md)). Retain in-process store-exe, shell/context, ownership index, and CAS metadata across source-only generations. `NXR_WATCH_PREWARM=off` kill-switch. `NXR_PERF_STATS` schema **v9** adds `watch_prewarm_*` counters.
- Child output event batching + terminal write coalescing (perf Wave 7a + 7b; [ADR-0162](docs/adr/0162-child-output-batching.md)). Adjacent pipe reads coalesce before chunk events; live mode batches terminal writes.
- Optional process log broker via `nxrd` (perf Wave 7c; [ADR-0164](docs/adr/0164-process-log-broker.md)). `log.open` / `log.append` / `log.subscribe` / `log.close` on the daemon socket; `nxr process logs --follow` prefers the broker and falls back to the 200 ms file poll when the daemon is absent. Kill-switch: `NXR_LOG_BROKER=off`. Bounded in-memory tails (≤256 KiB/stream).
- Determinate discovery/evaluation strategy planner ([ADR-0165](docs/adr/0165-determinate-eval-strategy.md)). `plan_discovery_eval` selects coalesced parallel eval, lazy-trees compatible, or compatibility paths from capability probes; `NXR_EVAL_STRATEGY=compatibility` kill-switch. `nxr cache explain` reports `discovery_eval_strategy`.
- Optional `nxrMetadata.<system>` single-eval discovery endpoint ([ADR-0166](docs/adr/0166-nxr-metadata-endpoint.md)). flake-parts emits a compact index; cold discovery prefers one targeted eval then falls back to coalesce/show+eval. `NXR_NXR_METADATA=off` kill-switch. Output is never required; standard flake outputs remain authoritative.
- Batched Nix store path queries ([ADR-0167](docs/adr/0167-batched-store-queries.md)). `store_query` batches `nix path-info --json` for store-exe validation when lazy trees are not disabled; `NXR_STORE_QUERIES=fs` kill-switch. Falls back to filesystem checks on failure.
- Experimental optional Nix eval worker via `nxrd` (perf Wave 8c; [ADR-0168](docs/adr/0168-experimental-eval-worker.md)). Opt-in `NXR_EVAL_WORKER=1` on Determinate-eligible hosts; `eval.prepare` / `eval.get` / `eval.put` retain metadata/tasks/list JSON across invocations. Default path unchanged; always falls back to subprocess `nix eval`. Not required for correctness.

### Changed

- Workspace and Nix package version **3.2.0**.

## [3.1.4] - 2026-07-28

Workspace cache safety before heavier CAS/context use.

### Added

- `cache.secretPolicy` (`disable` | `ignore-values`) for expert override of
  secret-bearing workspace-cache policy.

### Fixed

- Secret-bearing tasks (secret env inputs / context secrets) disable workspace
  cache by default; `nxr cache explain` surfaces the reason ([#1](https://github.com/willmortimer/nxr/issues/1)).
- `cache.mode` `shared` / `shared-read` fail closed until a shared CAS transport
  exists ([#2](https://github.com/willmortimer/nxr/issues/2)).
- `fixtures/README.md` lists `workspace-cache`, `processes`, and `inventory-custom`.

### Changed

- Workspace and Nix package version **3.1.4**.

## [3.1.3] - 2026-07-28

Correctness hardening for the 3.1 workspace-actions / process surface.

### Added

- Flake-parts options for 3.1 task fields (inputs/outputs/cache/resources) and
  processes, with `checks.*.flake-parts-v2-fields` and a fixture flake.
- `checks.<system>.cli-ref` fails CI when generated CLI help drifts from Clap.

### Changed

- Regenerated `docs/CLI_GENERATED.md`; README and schema v2 status matrix aligned
  with 3.1 shipped CLI and experimental workspace cache / process preview.
- Workspace and Nix package version **3.1.3**.

### Fixed

- Scheduler no longer hangs when cache hits complete work: ready successors are
  accumulated into `to_start` after `complete()`.
- Unschedulable CPU/memory requests are rejected at `Scheduler::new`.
- Process commands propagate flake context; validate process names; strengthen
  PID start-time identity on Linux/macOS.
- Workspace action keys include args/argv, task definition, relative cwd, env
  state, and glob material previously omitted.
- CAS protocol v2: atomic publish, restore modes, optional outputs, and
  symlink containment.
- `cargo deny` allowlist includes `BSD-2-Clause` (`arrayref` via `blake3`).
- Detect `nxr` in `nix flake show --json` when reported as `{ "type": "unknown" }`
  so unprefixed `watch` still prefers tasks over same-named apps.
- Hermetic process tests resolve Unix utilities via `PATH` and avoid `perl`.
- Watch first-generation config probe budget allows the `show-config` fallback;
  source-restart integration tests hardened for macOS FSEvents.
- `nxr plan <app>` on app-only flakes no longer fails with `unknown selector or task`
  (bare names were expanded as task selectors before app resolution).
- `nxr task` bare names keep exit code 6 / `unknown task root` (selector expansion no
  longer maps unknown tasks to usage exit 2).
- `--refresh-discovery` is honored on the task execution path (was hardcoded off in
  `WorkspaceSnapshot`, so coalesced refresh tests and CLI refresh were no-ops).
- Hermetic `checks.*.test` skips live Nix capability-cache probes under
  `NXR_SKIP_NIX_INTEGRATION` (sandbox cannot write `/nix/var/nix/profiles`).

## [3.1.2] - 2026-07-27

### Fixed

- rustfmt drift in 3.1 workspace/cache/history/coalesce sources so CI format checks pass.
- Release smoke README assertion mismatched the nix-package archive text (`runnable standalone`), which blocked GitHub Release publish after v3.1.1 builds succeeded.

### Changed

- Workspace and Nix package version **3.1.2**.

## [3.1.1] - 2026-07-27

### Fixed

- Include `templates/` in the hermetic workspace source filter so `nxr init`
  `include_str!` paths compile under `nix build .#nxr` (broke package/SBOM
  builds from v2.6 through v3.1.0).
- Add `checks.*.workspace-src-includes` to fail CI if future `include_str!` /
  `include_bytes!` targets are omitted from the filter.

### Changed

- Workspace and Nix package version **3.1.1**.

## [3.1.0] - 2026-07-27

Workspace actions (“Nix Turborepo”) MVP plus process and inventory foundations.

### Added

- Two-tier actions and local workspace CAS with `nxr cache explain <task>` (ADR-0147).
- Resource-aware scheduling (exclusive locks + soft CPU/memory pools).
- Process MVP: `nxr up` / `status` / `logs` / `down`.
- `nxr inventory` / `--role` and inspect inventory entries.
- `nxr history list|clear`; coalesced cold discovery when Determinate parallel eval is available.

### Changed

- Workspace and Nix package version **3.1.0**.

## [3.0.0] - 2026-07-27

Secure execution contexts: full env policy, trust, secret bindings/delivery, and context CLI.

### Added

- Full clean/inherit keep/set/unset environment policy at spawn; schema v2 semantic validation.
- Project trust database (`nxr trust status|add|revoke`, `NXR_TRUST_PROJECT`).
- User secret bindings (`secret-bindings.toml` / config.toml); file and stdin delivery; sops path stubs.
- `nxr context list|inspect|run`.

### Changed

- Workspace and Nix package version **3.0.0**.

## [2.8.0] - 2026-07-27

Automation ergonomics (ADR-0148): scaffolding, selectors, CI plan export, and reports.

### Added

- `nxr init` templates: `rust`, `node`, `mixed`, `monorepo`.
- `nxr migrate justfile` / `nxr migrate mise` (suggest Nix; never execute recipes).
- Task selectors `category:<name>` and `changed` (affected alias) on list/task/plan.
- `nxr ci plan --json` (`ci-plan-v1` schema).
- Opt-in task reports: JUnit, SARIF, coverage/benchmark JSON stubs (`--report` / `--junit`).
- `fixtures/golden` reference flake; `docs/CLI_GENERATED.md` via `cargo xtask cli-ref`.

### Changed

- Workspace and Nix package version **2.8.0**.

## [2.7.1] - 2026-07-27

Correctness release: closes the 2.7 batch that was never tagged as 2.7.0, plus
audit-driven mio/schema/cache/context fixes (ADR-0143–0146, ADR-0149).

### Fixed

- Mio pipe multiplexing: `O_NONBLOCK`, drain-until-WouldBlock with 1 MiB
  fairness budget, keep pipes until EOF after process exit, propagate poll
  errors (ADR-0143).
- Capability cache (schema **v4**): env-layer digest includes Nix config file
  identity (size/mtime/ctime/content), not only env-var strings (ADR-0145).
- DeadlineQueue nearest-deadline lookup is O(log n) via lazy cancelled prune.
- Context `confirm` / `shell` are enforced (TTY/`NXR_ASSUME_YES`, develop wrap)
  instead of silently ignored (ADR-0149).

### Added

- Portable release archives `nxr-<version>-<system>-portable.tar.gz` (ADR-0141)
  alongside labeled `*-nix-package.tar.gz` layouts.
- `nxr watch app:<name>` / `task:<name>` disambiguation; `nxr run --watch`
  resolves apps without loading tasks; unprefixed watch skips task eval when the
  name is app-only.
- Synthetic monorepo fingerprint warm-path bench
  (`scripts/perf/measure-fingerprint.sh`).
- mio-backed pipe multiplexing for piped task stdout/stderr (Unix).
- Expanded `nxr doctor determinate` finding IDs with warm capability-cache reuse.
- Task schema **v2** strict parse; `perSystem.nxr.contexts`; env-provider secrets
  at spawn; optional secret `provider` (default `env`).
- Flake-parts auto-emits `schema_version: 2` when contexts/security fields are
  present (ADR-0144); `nxr.schemaVersion` override with fail-closed rules.
- Audit ADRs 0143–0150 and remapped roadmap through 3.1.

### Changed

- Capability cache schema **v3→v4**; warm hits still skip all probes when the
  env digest (now including conf files) matches.
- Fingerprint index: skip unchanged rewrite; compact serialization; ctime;
  `NXR_FINGERPRINT_FORCE_REHASH_SECS`.
- CI runs `nix flake check -L` on ubuntu/latest; warm-path threshold enforcement.
- Workspace and Nix package version **2.7.1**.

## [2.6.0] - 2026-07-26

Feature release: warm-path latency foundations and ecosystem ergonomics.

### Added

#### Latency and discovery

- Persistent Nix **capability cache** (keyed by executable identity); warm
  `list`/`plan` skip version/config/help reprobes (`NXR_CAPABILITY_CACHE`,
  `nxr cache status`/`clear` cover discovery + capabilities).
- **Incremental fingerprint index** for warm discovery (cache schema v5);
  unchanged `.nix` files are not re-read.
- Per-invocation **`WorkspaceState`** so doctor/watch avoid double adapter init.
- Watch **source-only snapshot reuse** (metadata edits still rediscover).
- Generic flake **inventory AST** (legacy + Determinate inventory v2).
- **`exportedSchemas.nxr`** and optional `flake-schemas` merge via the
  flake-parts module.
- **`nxr doctor determinate`** with stable finding IDs and redaction.
- Release perf harness (`scripts/perf/measure-release.sh`) and recorded p50
  baselines (warm fixture `list`/`plan` ≈ 10 ms).
- Draft **task schema v2** docs/schema only (`schemas/task-v2.schema.json`,
  `docs/TASK_SCHEMA_V2.md`) — not implemented by the runner.

#### Ecosystem ergonomics

- **`homeManagerModules.default`** (`programs.nxr`: package, completions,
  `config.toml` defaults, optional direnv; no `homeConfigurations` / secrets).
- **`nxr fmt`** — thin `nix fmt` wrapper.
- **`nxr in <shell> <target>`** — ergonomic `--shell` form (reserved command).
- **`nxr envrc`** / `--write` / `--force` — generate `.envrc` only (never
  activates direnv).
- **`nxr doctor env`**, **`doctor cache`**, **`doctor builders`**.
- Generic **`nxr build`** installables and **`--attr`** escape hatch.
- Read-only **configuration** adapters: `list` / `inspect` / `build
  configuration` for `nixosConfigurations` / `darwinConfigurations` /
  `homeConfigurations` (build only; never switch/activate).

### Changed

- Workspace and Nix package version **2.6.0**.
- README / CLI contract / reference updated for the new surface.
- Active roadmap advances past 2.6 shipped items toward 3.0 execution-context
  schema.

## [2.5.0] - 2026-07-26

Feature release: affected execution for tasks and plans.

### Added

- `nxr task --affected` runs the union DAG of affected tasks (same path sources
  and strict policy as `nxr affected`). Optional task names intersect the
  affected set; an empty set is a successful no-op.
- `nxr plan --affected` emits the multi-root execution plan for that set.
- Path sources on both commands: `--base`, `--working-tree`, `--all-changes`,
  and `--path` (repeatable). Conflicts with `task --watch`.

### Changed

- Workspace and Nix package version **2.5.0**.

## [2.4.1] - 2026-07-21

Patch release: finish the 2.4 run model so timeouts, summaries, events, and
shell completion match the documented surface.

### Fixed

- `CompleteTarget` unit test covers all seven completion targets.
- Simultaneous task timeouts under fail-fast no longer double-complete a peer
  that was already shut down; keep-going skips nodes no longer running.
- `--output summary` prints the documented header and includes skipped /
  pre-launch-cancelled plan nodes (not only processes that started).
- Structured event fields (`run_id`, `seq`, timestamps, run/node durations) are
  populated via a `RunEventDecorator` around the event sink.
- Generated Bash/Zsh/Fish completion routes to `__complete` targets by command
  position (tasks, packages, checks, shells, namespaces, categories).

### Added

- Flake-parts `timeout` and `terminationGracePeriod` task options (emitted into
  `nxr.<system>`).
- `fixtures/task-timeout` for timeout evaluation and dual-timeout runs.

### Changed

- Workspace and Nix package version **2.4.1**.
- README / TASKS docs: multi-root watch, `--output summary`, timeout fields.

## [2.4.0] - 2026-07-20

Feature release: structured run results, per-task timeouts, richer completion,
and `--output summary`.

### Added

- Event fields (additive): node/run `status`, `duration_ms`, optional timestamps,
  `reason`, `seq`, and `run_id` on plan/run envelopes.
- `--output summary` prints a per-node status/duration table.
- Optional task `timeout` and `terminationGracePeriod` (e.g. `10m`, `5s`) with
  supervisor timeout enforcement (`timed_out` outcome).
- Dynamic completion targets: `apps`, `tasks`, `packages`, `checks`, `shells`,
  `namespaces`, `categories`.
- Duration parsing helpers (`parse_duration` / `format_duration`).

### Changed

- Workspace and Nix package version **2.4.0**.
- Docs mark summary / timestamps / timeouts as shipped where implemented.

## [2.3.3] - 2026-07-20

Correctness cut: watch parity with the normal task pipeline, empty-affected
semantics, path safety, catalog decoupling, and stable cache digests. No new
product features.

### Fixed

- `task --watch` / `nxr watch` for tasks use WorkspaceSnapshot → ExecutionPlan →
  PreparedTaskNode → Scheduler (preserve `-j`, `--keep-going`, working
  directories, multi-root, `--output` / `--events`, and real exit codes).
- Mid-run filesystem changes abort the current task generation and rebuild.
- Valid empty affected diffs classify every node as unaffected (strict lists
  empty).
- `list apps` and completion no longer require optional `nxr` task metadata;
  tasks remain best-effort for `discoveryInputs` when available.
- Repository-relative validation for `discoveryInputs`, task `paths`, affected
  path roots, and explicit `nxr affected` path args (no absolute / `..`).
- Discovery cache fingerprints and file names use BLAKE3 hex digests
  (schema **v4**); `DefaultHasher` is no longer persisted.
- Docs no longer present unimplemented `--output summary` / timestamps as
  shipped V2 surface.

### Changed

- Workspace and Nix package version **2.3.3**.

## [2.3.2] - 2026-07-20

Hardening patch: transparent TTY stderr, colder completion cache honesty, and
affected edge-case correctness. No new features.

### Fixed

- Foreground / named build-check-shell: inherit stderr on a TTY (no capture);
  non-TTY paths tee with a bounded ~128 KiB rolling tail for suggestions.
- Cold completion evaluates apps together with the lightweight `nxr` document
  (`require_tasks`) so `discoveryInputs` enter the first cache entry.
- `nxr affected` with a valid empty path source succeeds (empty lists) instead
  of a usage error; missing path source remains a usage error.
- Git path collection uses `--name-status -z --find-renames` and includes both
  sides of rename/copy records.
- Affected `nodes` includes every classified graph node (including unaffected).
- Release matrix asserts tag version equals package version and checks archive
  layout on every platform build.

### Changed

- Workspace and Nix package version **2.3.2**.

## [2.3.1] - 2026-07-20

Trust-and-latency patch: one-process bare apps, sounder discovery cache, strict
user Nix flags, safer affected analysis, and Nix-equipped release archives.

### Added

- Discovery cache **v3**: content hashes for `*.nix` / `flake.lock`, Nix
  identity, discovery-schema version, `perSystem.nxr.discoveryInputs`, and a
  TTL backstop (`NXR_CACHE_TTL_SECS`).
- `nxr affected` schema **v2**: `affected` / `unaffected` / `unknown`; default
  **strict** policy includes `unknown` (`--strict` / `--no-strict`).
- Path modes: `--working-tree` and `--all-changes <ref>` alongside `--base`.
- Release extract-smoke job; archives include man, completions, and shell
  integration assets (Nix-equipped hosts).

### Changed

- Bare `nxr <app>` / `nxr run` locate `nix` only — no capability probes unless
  `--offline` / `--accept-flake-config`; suggestion discovery only on
  installable-resolution stderr.
- Named `build` / `check` / `shell` use direct installables (no whole-output
  discovery).
- Explicit `--offline` / `--accept-flake-config` fail when unsupported (never
  silently dropped); internal `--no-write-lock-file` stays best-effort.
- Fixtures are self-contained (pinned `nixpkgs`, inline `nxr.<system>`); no
  `path:../..` of this repo.
- Grouped/failure-only output spills to temp files above a size threshold.
- Workspace and Nix package version **2.3.1**.

### Fixed

- Determinate Nix flakes detection when `experimental-features` omits `flakes`.
- Parse Determinate Nix `flake show --json` inventory v2 (`what` /
  `shortDescription`) in addition to upstream legacy `type` / `description`.
- Named `build` / `check` / `shell` restore close-match suggestions after a
  missing-attribute Nix failure (still skip discovery on the happy path).
- Process-group escalation tests avoid nested `sleep` under `trap '' TERM` so
  Linux CI does not see zombie PGIDs after SIGKILL.
- Release smoke `cmp`s the uploaded archive binary against a local `nix build`
  (Nix ELFs need their store closure; extract alone is not executable).
- Local flake roots passed as `path:<absolute>` URIs.
- `workingDirectory` rejects parent traversal and must stay under the flake root.
- Combined output+events uses the supplied stderr writer.
- Unknown `nxr.projects.json` members surface as `doctor` warnings.
- Invalid affected globs mark nodes `unknown`; dependency reasons accumulate.

## [2.3.0] - 2026-07-19

V2.3 monorepo ergonomics release: filtered views for large operation sets, conservative affected analysis, and optional ecosystem graph adapters.

### Added

- Namespaced list/inspect views: `--category` filters apps (via `nxr.category`
  listing metadata) and tasks; `--namespace` filters by optional
  `nxr.projects.json` membership. Flake apps remain the operation authority;
  see [docs/MONOREPO_VIEWS.md](docs/MONOREPO_VIEWS.md).
- Optional additive `apps` map on `nxr.<system>` for app listing categories;
  flake-parts `nxr.apps.<name>.category` emits it.
- `nxr affected [--base <ref>] [PATH…]` for conservative path-based affected
  analysis over apps and tasks (`--json` for CI). Tasks may declare `paths`
  roots; changes propagate through `dependsOn` edges.
- Thin ecosystem graph adapter boundary: read-only relationship metadata in
  `nxr-core`, documented in [docs/ADAPTERS.md](docs/ADAPTERS.md) (adapters are
  non-authoritative; flake apps stay canonical).
- Schemas: `projects-v1`, `affected-v1`, and `ecosystem-graph-v0`.
- Fixtures: `namespaced-monorepo/`, `affected-deps/`, and
  `ecosystem-graph-cargo/`.

### Changed

- Workspace and Nix package version **2.3.0**.
- Release workflow: build `aarch64-linux` on native `ubuntu-24.04-arm`; update
  `cargo-cyclonedx` invocation for current CLI (`--describe binaries`).

## [2.2.0] - 2026-07-19

V2.2 flake UX release: standard flake output commands, richer diagnostics, and task ergonomics.

### Added

- Flake output command plane: `nxr list [apps|checks|packages|shells|tasks]`,
  `nxr build [name]`, `nxr check [name]`, and `nxr shell [name]` map to native
  Nix operations (`nix build` / `nix flake check` / `nix develop`) using the
  same `flake show` discovery path as apps.
- `nxr explain <app|task>` and `nxr explain app|task <name>` for resolution and
  exact Nix invocation diagnostics.
- `nxr doctor --all` for non-destructive workspace findings (app descriptions,
  naming, discovery cache).
- Multi-root task union: pass multiple task names to `nxr task` to run the union
  of their dependency subgraphs (shared deps run once).
- Interactive task exclusivity: `interactive = true` tasks inherit stdin/TTY, run
  alone, and reject `--output` / `--events`.

### Changed

- Workspace and Nix package version **2.2.0**.

## [2.1.0] - 2026-07-19

V2.1 trustworthiness release: predictable discovery and execution on real flakes, with CI hardening and release artifacts.

### Added

- `WorkspaceSnapshot`: evaluate the flake once per run; bare-app `nix run` fast path.
- Real Nix capability negotiation (`NixCapabilities`) for doctor and the adapter.
- `nxr cache clear` and `nxr cache status` for discovery cache management.
- Nix argv forwarding: `--offline`, `--accept-flake-config`, `--nix-option KEY=VAL`, and repeatable `--nix-arg`.
- `--output raw` for byte-safe task output without UTF-8 loss on binary streams.
- `--shell-mode smart|always|never` (default `smart`) for nested-shell identity skip.
- Zero-boilerplate `shellIntegration.package` default from the flake module.
- Four-system Nix baseline (`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`) and expanded flake check suite.
- CI: packaged-binary smoke tests against fixtures, multi-Nix matrix, and pinned third-party actions.
- Release artifacts with checksums and SBOM.
- Process supervision invariant tests (signal escalation, orphan prevention).
- V2.x bridge: [`schemas/events-v1.schema.json`](schemas/events-v1.schema.json) aligned with Rust `Event`, extension-point notes in [COMPATIBILITY.md](docs/COMPATIBILITY.md), and a timed large-DAG scheduler CI budget test.

### Changed

- Discovery cache bypass renamed from `--refresh` to `--refresh-discovery`. Use `--nix-arg --refresh` to forward Nix's `--refresh` global.
- Task `workingDirectory` honored with CLI precedence.
- Recursive `.nix` fingerprint for discovery cache invalidation (edits under imported files invalidate without touching `flake.nix`).
- Serialized discovery cache writes under an exclusive lock.
- Root [README](README.md) retargeted for flake consumers; maintainer/dev content moved to [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
- Demo GIF covers list/run, inspect/graph (mermaid+dot), task aliases, parallel `-j`, `--shell`, and watch (`docs/demo/nxr.tape`).
- Workspace and Nix package version **2.1.0**.

### Migration

- Replace `nxr --refresh …` with `nxr --refresh-discovery …` for nxr's discovery cache bypass. To pass Nix's own `--refresh`, use `nxr --nix-arg --refresh …`.

## [2.0.0] - 2026-07-19

V2.0 orchestration release (Phases 7–15): parallel task DAG, supervisor, watch v2, shell integration, and schema freeze.

### Added

- Orchestration V2 core: `ExecutionPlan` + typed events, multi-child `Supervisor`, parallel `nxr task -j` / `--keep-going` (fail-fast default), `--output live|grouped|failures`, `--events jsonl`, global `--shell`.
- Watch v2: `--include` / `--exclude` globs, `--clear`, `nxr run|task --watch` aliases, Supervisor-backed generation shutdown.
- Task UX: optional `aliases`, shared `resolve_task_name`, `nxr plan` app-first then task `ExecutionPlan`, `list`/`inspect --category`, hidden-task filtering.
- Fixtures: `fixtures/parallel-group/`, `fixtures/named-dev-shells/`.
- Flake-parts `shellIntegration` module: `nxr` + session hooks under `share/nxr/shell/`.
- `nxr graph --format dot` for stable Graphviz output.
- Soak/stress tests: watcher debounce burst coalescing, supervisor multi-child TERM→KILL, large synthetic DAG scheduler smoke.
- **Schema freeze (V2.0):** `task-v1`, `execution-plan-v1`, and events vocabulary documented in [COMPATIBILITY.md](docs/COMPATIBILITY.md); `events-v1` JSON schema published in the V2.x bridge (see [2.1.0]).

### Changed

- README documents parallel tasks, shell, watch v2, and V2.0 status relative to [ROADMAP.md](docs/ROADMAP.md).
- [CLI_REFERENCE.md](docs/CLI_REFERENCE.md) and [TASKS.md](docs/TASKS.md) cover the new flags, schema fields, argument/stdin freeze, and V2 migration notes.
- Workspace and Nix package version **2.0.0**.

## [1.0.0] - 2026-07-18

V1.0 standard flake app runner (Phases 0–6 complete).

### Added

- Man page `nxr(1)` via `clap_mangen` (`nxr __manpage`; installed by `nix build .#nxr`).
- [Performance baselines](docs/PERFORMANCE.md) for list/cache/completion.
- [V1 security review](docs/SECURITY_REVIEW_V1.md) against ARCHITECTURE §8.
- Direnv/session-local shell completion wiring (`.envrc`, `shell/direnv-zsh-hook.zsh`).

### Changed

- Workspace and Nix package version **1.0.0**.

## [0.1.0] - 2026-07-18

First taggable V1 prerelease: a standard Nix flake app runner through Phase 5 of the [roadmap](docs/ROADMAP.md).

### Added

#### Phase 0 — foundation

- Rust workspace and `nxr` CLI package.
- Nix flake for development, packaging (`nix build .#nxr`), and contributor apps (`fmt`, `lint`, `test`, `deny`).
- Fixture flakes under `fixtures/` for discovery and execution smoke tests.
- CI on Ubuntu (`x86_64-linux`) and macOS (`aarch64-darwin`).
- Architecture decision record index and project documentation contract.

#### Phase 1 — discovery and listing

- Upward `flake.nix` discovery from the invocation directory.
- Explicit `--flake` for local and remote flake references.
- `nxr` / `nxr list` with human output and `nxr list --json`.
- Normalized app model with descriptions and default-app detection.
- Nix executable detection and evaluation diagnostics.

#### Phase 2 — foreground execution

- `nxr <app>` and `nxr run <app>` with exact argument forwarding.
- `--` stripping; no shell evaluation of app arguments.
- Current-directory preservation; `--root` and `--cwd`.
- Exit-code and signal propagation; TTY inheritance.
- `nxr plan <app>` and `--dry-run` for inspectable Nix commands.

#### Phase 3 — ergonomic discovery

- Shell completion for Bash, Zsh, and Fish (`nxr completion <shell>`).
- Interactive fuzzy selector (`nxr select`, `nxr --select`).
- App-not-found suggestions.
- Discovery metadata cache with `--refresh` invalidation.

#### Phase 4 — output and diagnostics

- Human, plain, and JSON runner output modes.
- Quiet and verbose levels; `--no-color` and `--color`.
- Stable exit codes and sanitized flake metadata in terminal output.
- Machine-readable plan JSON schema.

#### Phase 5 — doctor and app authoring

- `nxr doctor` and `nxr doctor --clean-env` for environment validation.
- Nix `mkApp` helper and flake-parts app module.
- [App authoring guide](docs/APP_AUTHORING.md) and [migration how-to](docs/MIGRATE_FROM_MISE_JUST.md) from mise, just, and shell aliases.

#### Release scaffolding (Phase 6, partial)

- Version `0.1.0` workspace and Nix package.
- [Compatibility matrix](docs/COMPATIBILITY.md), [CLI reference](docs/CLI_REFERENCE.md), and [telemetry decision](docs/TELEMETRY.md) (default: none).
- Tag-triggered [release workflow](.github/workflows/release.yml) (quality gate only; no publish secrets).

[2.6.0]: https://github.com/willmortimer/nxr/compare/v2.5.0...v2.6.0
[2.5.0]: https://github.com/willmortimer/nxr/compare/v2.4.1...v2.5.0
[2.4.1]: https://github.com/willmortimer/nxr/compare/v2.4.0...v2.4.1
[2.4.0]: https://github.com/willmortimer/nxr/compare/v2.3.3...v2.4.0
[2.3.3]: https://github.com/willmortimer/nxr/compare/v2.3.2...v2.3.3
[2.3.2]: https://github.com/willmortimer/nxr/compare/v2.3.1...v2.3.2
[2.3.1]: https://github.com/willmortimer/nxr/compare/v2.3.0...v2.3.1
[2.3.0]: https://github.com/willmortimer/nxr/compare/v2.2.0...v2.3.0
[2.2.0]: https://github.com/willmortimer/nxr/compare/v2.1.0...v2.2.0
[2.1.0]: https://github.com/willmortimer/nxr/compare/v2.0.0...v2.1.0
[2.0.0]: https://github.com/willmortimer/nxr/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/willmortimer/nxr/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/willmortimer/nxr/compare/v0.0.0...v0.1.0
