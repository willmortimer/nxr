# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[2.3.0]: https://github.com/willmortimer/nxr/compare/v2.2.0...v2.3.0
[2.2.0]: https://github.com/willmortimer/nxr/compare/v2.1.0...v2.2.0
[2.1.0]: https://github.com/willmortimer/nxr/compare/v2.0.0...v2.1.0
[2.0.0]: https://github.com/willmortimer/nxr/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/willmortimer/nxr/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/willmortimer/nxr/compare/v0.0.0...v0.1.0
