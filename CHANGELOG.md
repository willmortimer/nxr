# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
