# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Discovery cache bypass renamed from `--refresh` to `--refresh-discovery`. Use `--nix-arg --refresh` to forward Nix's `--refresh` global.
- Root [README](README.md) retargeted for flake consumers; maintainer/dev content moved to [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
- Demo GIF covers list/run, inspect/graph (mermaid+dot), task aliases, parallel `-j`, `--shell`, and watch (`docs/demo/nxr.tape`).

### Added

- `nxr cache clear` and `nxr cache status` for discovery cache management.
- Nix argv forwarding: `--offline`, `--accept-flake-config`, `--nix-option KEY=VAL`, and repeatable `--nix-arg`.
- V2.x bridge (Phase 16 slim): [`schemas/events-v1.schema.json`](schemas/events-v1.schema.json) aligned with Rust `Event`, extension-point notes in [COMPATIBILITY.md](docs/COMPATIBILITY.md), and a timed large-DAG scheduler CI budget test.

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
- **Schema freeze (V2.0):** `task-v1`, `execution-plan-v1`, and events vocabulary documented in [COMPATIBILITY.md](docs/COMPATIBILITY.md); `events-v1` JSON schema published in the V2.x bridge (see Unreleased).

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

[2.0.0]: https://github.com/willmortimer/nxr/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/willmortimer/nxr/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/willmortimer/nxr/compare/v0.0.0...v0.1.0
