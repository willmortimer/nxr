# nxr

Zero-configuration command interface for standard Nix flake apps.

Treat flake apps as the project's executable public interface — `nxr test` is the ergonomic form of `nix run .#test`. Optional task graphs orchestrate those apps in parallel with labeled output, watch/restart, and named-shell execution.

<p align="center">
  <img src="docs/demo/nxr.gif" alt="nxr demo: list apps, run hello, graph a task, dry-run ci" width="980" />
</p>

<p align="center">
  <em>Animated terminal (GIF) recorded with <a href="https://github.com/charmbracelet/vhs">VHS</a> — regenerate via <code>./docs/demo/record.sh</code></em>
</p>

## Quick start

```bash
nix develop          # optional: this repo's shell
nxr list             # discover apps for the current flake
nxr test             # ≈ nix run .#test
nxr fixtures/basic-apps#hello   # inline flake#app
```

## Docs

| Doc | Topic |
|---|---|
| **[docs/INDEX.md](docs/INDEX.md)** | Documentation map |
| [docs/CONTRACT_SUMMARY.md](docs/CONTRACT_SUMMARY.md) | Locked product decisions |
| [docs/CLI_REFERENCE.md](docs/CLI_REFERENCE.md) | Commands and globals |
| [docs/APP_AUTHORING.md](docs/APP_AUTHORING.md) | `mkApp` / `mkScriptApp` / `mkPackageApp` |
| [docs/TASKS.md](docs/TASKS.md) | `perSystem.nxr.tasks` → `nxr.<system>` |
| [docs/ROADMAP.md](docs/ROADMAP.md) | V1 → V3 delivery plan |

## What works today

### Discover and run

```bash
nxr                              # list apps (+ tasks when present)
nxr list --json
nxr list --category ci           # filter tasks by category
nxr hello                        # bare form ≈ nix run .#hello (apps only)
nxr run hello -- --flag
nxr plan hello --json            # app plan
nxr plan ci --json               # task ExecutionPlan when name is not an app
nxr select                       # interactive picker
nxr fixtures/basic-apps#hello    # inline flake#app
nxr --flake fixtures/basic-apps hello
nxr --shell default hello        # via nix develop .#default -c …
```

### Diagnostics and environment

```bash
nxr doctor
nxr doctor --all
nxr doctor --clean-env hello
nxr --clean-env --keep-env HOME run hello
nxr --log-format json list
```

### Completions

```bash
nxr completion zsh
# In this repo, direnv materializes hooks under .direnv/ (see .envrc).
```

Flake consumers can opt into dev-shell integration with `perSystem.nxr.shellIntegration` (see [docs/DEV_ENV_INTEGRATION.md](docs/DEV_ENV_INTEGRATION.md)).

### Tasks and graphs

Declare tasks with `nxr.flakeModules.default` (`perSystem.nxr.tasks`). See [docs/TASKS.md](docs/TASKS.md).

```bash
nxr --flake fixtures/task-dag inspect
nxr --flake fixtures/task-dag inspect task ci
nxr --flake fixtures/task-dag task ci
nxr --flake fixtures/task-dag task ci -j 4          # parallel ready-set
nxr --flake fixtures/task-dag task ci --keep-going  # opt-in (fail-fast default)
nxr --flake fixtures/task-dag task ci --dry-run
nxr --flake fixtures/task-dag --output grouped task ci
nxr --flake fixtures/task-dag --events jsonl task ci
nxr --flake fixtures/task-dag graph ci
nxr --flake fixtures/task-dag graph ci --format mermaid
```

Explicit commands (`task` / `graph` / `inspect task` / `watch` / task-side `plan`) resolve **aliases**. Bare `nxr <name>` stays **app-only**.

### Watch

```bash
nxr watch hello                              # watch flake root; kill+rerun
nxr watch ci --debounce 500                  # task chain (task-first)
nxr watch hello --include 'src/**' --exclude '**/*.md' --clear
nxr run hello --watch
nxr task ci --watch --debounce 500
```

Built-in ignores: `.git`, `target`, `result*`, `/nix/store`. Ctrl-C stops the watcher and shuts down the current generation.

### Nix authoring helpers

From `flake.lib` / `nxr.flakeModules.default`:

- `mkApp` / `mkScriptApp` — shell-backed flake apps
- `mkPackageApp` — wrap an existing package binary as an app
- `perSystem.nxr.apps` / `perSystem.nxr.tasks` — declarative apps and task metadata (aliases, categories, `dependsOn`, …)

See [examples/mk-app](examples/mk-app/) and [docs/APP_AUTHORING.md](docs/APP_AUTHORING.md).

## Project apps

Same operations CI runs:

```bash
nix build .#nxr          # package the CLI
nix run .#fmt            # rustfmt (add -- --check in CI)
nix run .#lint           # clippy -D warnings
nix run .#test           # cargo nextest
nix run .#deny           # cargo-deny
```

## How we test

1. **Repo quality apps** — `fmt` / `lint` / `test` / `deny` (and `.github/workflows/ci.yml`).
2. **Fixture flakes** under [`fixtures/`](fixtures/README.md) — `hello`, `echo-args`, `fail`, `pwd`, metadata, nested dirs, `task-dag`, `parallel-group`, `named-dev-shells`.

```bash
nix run ./fixtures/basic-apps#hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
cargo run -p nxr-cli -- --flake fixtures/task-dag task ci -j 2 --dry-run
cargo run -p nxr-cli -- --flake fixtures/task-dag graph ci --format mermaid
```

## Demo GIF

The README animation is a **GIF** (not asciinema). It is produced from a checked-in [VHS](https://github.com/charmbracelet/vhs) script so it stays reproducible:

```bash
./docs/demo/record.sh    # needs: vhs, ffmpeg, ttyd, nix
```

| File | Role |
|---|---|
| [docs/demo/nxr.tape](docs/demo/nxr.tape) | Recording script |
| [docs/demo/nxr.gif](docs/demo/nxr.gif) | Output embedded above |
| [docs/demo/record.sh](docs/demo/record.sh) | Build `nxr` + run VHS |

## License

MIT — see [LICENSE](LICENSE).

## Status

**V2.0.0 ready** (no git tag yet) — V1 app runner plus orchestration V2: parallel `nxr task -j` / `--keep-going`, labeled `--output` + `--events jsonl`, multi-child supervisor, `--shell`, watch globs/`--clear`/`run|task --watch`, task aliases/categories/`plan` fallback, flake-parts shell integration, `graph --format dot`, and frozen `task-v1` / `execution-plan-v1` / `events-v1`.

V2.x bridge landed: published `events-v1` schema, extension-point notes in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md), and an in-process large-DAG schedule smoke budget. A Ratatui “lazygit-style” dashboard is **V3.5 / Phase 35** in [docs/ROADMAP.md](docs/ROADMAP.md) — see that doc before adding a TUI crate. Full history: [CHANGELOG.md](CHANGELOG.md).
