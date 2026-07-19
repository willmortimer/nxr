# nxr

Zero-configuration command interface for standard Nix flake apps.

Treat flake apps as the project's executable public interface — `nxr test` is the ergonomic form of `nix run .#test`.

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

## What works today

### Discover and run

```bash
nxr                              # list apps
nxr list --json
nxr hello                        # bare form ≈ nix run .#hello
nxr run hello -- --flag
nxr plan hello --json
nxr select                       # interactive picker
nxr fixtures/basic-apps#hello    # inline flake#app
nxr --flake fixtures/basic-apps hello
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

### Tasks and graphs

Declare tasks with `nxr.flakeModules.default` (`perSystem.nxr.tasks`). See [docs/TASKS.md](docs/TASKS.md).

```bash
nxr --flake fixtures/task-dag inspect
nxr --flake fixtures/task-dag inspect task ci
nxr --flake fixtures/task-dag task ci
nxr --flake fixtures/task-dag task ci --dry-run
nxr --flake fixtures/task-dag graph ci
nxr --flake fixtures/task-dag graph ci --format mermaid
```

When the same name exists as both a **task** and an **app**, the task wins.

### Watch

```bash
nxr watch hello                  # watch flake root; kill+rerun on change
nxr watch ci --debounce 500      # task chain (task-first), 500ms debounce
```

Watches the local flake root (skips `.git`, `target`, `result*`). Ctrl-C stops the watcher and terminates the current generation.

### Nix authoring helpers

From `flake.lib` / `nxr.flakeModules.default`:

- `mkApp` / `mkScriptApp` — shell-backed flake apps
- `mkPackageApp` — wrap an existing package binary as an app
- `perSystem.nxr.apps` / `perSystem.nxr.tasks` — declarative apps and task metadata

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
2. **Fixture flakes** under [`fixtures/`](fixtures/README.md) — `hello`, `echo-args`, `fail`, `pwd`, metadata, nested dirs, `task-dag`.

```bash
nix run ./fixtures/basic-apps#hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
cargo run -p nxr-cli -- --flake fixtures/task-dag task ci --dry-run
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

**1.0.x** — standard flake app runner plus orchestration MVP: discover/list/run/plan/select/doctor/completion, inline `flake#app`, clean-env policy, inspect/task/graph/watch, and Nix app/task authoring helpers. See [CHANGELOG.md](CHANGELOG.md).
