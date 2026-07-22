# nxr

Ergonomic command plane for **standard Nix flake outputs**.

`nxr test` is the pleasant form of `nix run .#test`. Flake apps remain the
canonical leaf operations. `nxr` adds discovery, composition, diagnostics,
structured plans, supervision, and shell integration—without becoming another
task DSL, toolchain manager, or parallel implementation of Nix.

<p align="center">
  <img src="docs/demo/nxr.gif" alt="nxr demo: list, run, inspect, graph, parallel tasks, shell, and watch" width="980" />
</p>

## Install

```bash
nix profile install github:willmortimer/nxr#nxr
# or: nix shell github:willmortimer/nxr#nxr
```

For flake-parts projects, enable session-local completion and PATH wiring
(no duplicated package wiring required when the flake input is named `nxr`):

```nix
imports = [ inputs.nxr.flakeModules.default ];

perSystem.nxr = {
  shellIntegration.enable = true;
  # optional: shellIntegration.devShells = [ "default" "backend" ];
  tasks.ci = { app = "ci"; };
};
```

Details: [docs/DEV_ENV_INTEGRATION.md](docs/DEV_ENV_INTEGRATION.md).

## Quick start

From any directory under a flake:

```bash
nxr list                  # apps (+ tasks when present)
nxr list packages         # packages.<system>.*
nxr list checks
nxr list shells
nxr build                 # ≈ nix build .
nxr check fmt             # ≈ nix build .#checks.<system>.fmt
nxr shell                 # ≈ nix develop
nxr test                  # ≈ nix run .#test  (fast path; no flake show)
nxr select                # fuzzy picker
nxr plan test --json      # exact Nix argv + cwd / env / shell policy
nxr explain test          # why this app/task, cache key, capabilities, argv
nxr doctor --all          # environment + workspace findings
```

Inline flake + app (like `nix run`):

```bash
nxr ./path/to/flake#hello
nxr --flake ./path/to/flake hello
```

## Everyday commands

| Command | What it does |
|---|---|
| `nxr` / `nxr list` | List apps (and tasks) |
| `nxr list apps\|checks\|packages\|shells\|tasks` | List one catalog |
| `nxr list --category <name>` | Filter apps/tasks by category |
| `nxr list --namespace <name>` | Filter via optional `nxr.projects.json` |
| `nxr build [name]` | `nix build` for a package |
| `nxr check [name]` | Build a check, or `nix flake check` |
| `nxr shell [name]` | Interactive `nix develop` |
| `nxr <app> [args…]` | Run a flake app (apps only — not tasks) |
| `nxr run <app> [-- args…]` | Explicit run form |
| `nxr task <name>… [-j N]` | Run one or more task roots (union DAG; shared deps once) |
| `nxr graph <name>` | Print the plan (`--format text\|mermaid\|dot`) |
| `nxr watch <name>` | Kill + rerun on flake-root changes |
| `nxr plan <name>` | App plan, or task `ExecutionPlan` if not an app |
| `nxr explain <name>` | Full resolution + exact Nix invocation |
| `nxr affected [PATH…]` | Conservative path→app/task analysis (`--json` for CI) |
| `nxr inspect` / `doctor` | Overview and diagnostics |
| `nxr cache clear\|status` | Discovery cache management |
| `nxr completion zsh` | Shell completion script |

Useful globals: `--flake`, `--cwd` / `--root`, `--shell <name>`,
`--shell-mode smart|always|never`, `--clean-env`, `--refresh-discovery`,
`--offline`, `--nix-arg`, `--output live|grouped|failures|summary|raw`, `--events jsonl`.

Full index: [docs/CLI_REFERENCE.md](docs/CLI_REFERENCE.md).

### Tasks

Declare orchestration with `nxr.flakeModules.default` (`perSystem.nxr.tasks`).
Tasks coordinate apps; they do not replace them.

```bash
nxr task ci
nxr task lint unit integration -j 8   # union DAG; shared deps run once
nxr task ci --keep-going
nxr --output grouped task ci
nxr --output summary task ci
nxr --output raw task dev             # single child inherits stdio
nxr graph ci --format mermaid
```

Task fields worth knowing:

- `workingDirectory` — `invocation` | `flake-root` | relative path (CLI `--cwd`/`--root` win)
- `interactive = true` — exclusive TTY node; conflicts with multiplexed `--output` / `--events`
- `paths` — optional roots for `nxr affected`
- `timeout` / `terminationGracePeriod` — per-task wall-clock limits (e.g. `10m`, `5s`)
- `category` / aliases — listing and resolution helpers

Explicit commands (`task`, `graph`, `inspect task`, `watch`, task-side `plan`,
`explain task`) resolve **aliases**. Bare `nxr <name>` stays **app-only**.

Guide: [docs/TASKS.md](docs/TASKS.md).

### Dev shells

```bash
nxr --shell backend test              # wrap unless already in backend
nxr --shell-mode always --shell backend test
nxr --shell-mode never test
```

Smart mode reads `NXR_DEV_SHELL` from shell integration and skips redundant
`nix develop` nesting.

### Watch

```bash
nxr watch test
nxr watch ci --include 'src/**' --exclude '**/*.md' --clear
nxr run test --watch
nxr task ci --watch
nxr task lint unit --watch -j 4       # multi-root union + scheduler options
```

Built-in ignores: `.git`, `target`, `result*`, `/nix/store`. Ctrl-C stops the
watcher and shuts down the current generation.

### Monorepo views and affected

```bash
nxr list --category ci
nxr list --namespace web
nxr inspect --namespace api
nxr affected shared/lib.txt --json
nxr affected --base origin/main
```

Optional `nxr.projects.json` is **view-only**—flake apps remain the operation
authority. See [docs/MONOREPO_VIEWS.md](docs/MONOREPO_VIEWS.md) and
[docs/ADAPTERS.md](docs/ADAPTERS.md).

### Author flake apps

Prefer self-contained apps so `nxr` / `nix run` work outside a dirty shell:

- `mkApp` / `mkScriptApp` — shell-backed apps
- `mkPackageApp` — wrap an existing package binary

See [docs/APP_AUTHORING.md](docs/APP_AUTHORING.md) and [examples/mk-app](examples/mk-app/).

Coming from `mise` / `just`? [docs/MIGRATE_FROM_MISE_JUST.md](docs/MIGRATE_FROM_MISE_JUST.md).

## Documentation

| Doc | For |
|---|---|
| [docs/CLI_REFERENCE.md](docs/CLI_REFERENCE.md) | Commands and flags |
| [docs/APP_AUTHORING.md](docs/APP_AUTHORING.md) | Writing robust flake apps |
| [docs/TASKS.md](docs/TASKS.md) | Task graphs and aliases |
| [docs/MONOREPO_VIEWS.md](docs/MONOREPO_VIEWS.md) | Categories, namespaces, projects file |
| [docs/DEV_ENV_INTEGRATION.md](docs/DEV_ENV_INTEGRATION.md) | Dev shells, direnv, shellIntegration |
| [docs/EXECUTION_CONTEXT.md](docs/EXECUTION_CONTEXT.md) | Contexts, secrets, Home Manager, processes (planned) |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Active 2.5 → 3.1 plan |
| [docs/ADAPTERS.md](docs/ADAPTERS.md) | Read-only ecosystem graph boundary |
| [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) | Platforms and schema freeze |
| [docs/RELEASE.md](docs/RELEASE.md) | Release artifacts, checksums, SBOM |
| [docs/INDEX.md](docs/INDEX.md) | Full documentation map |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Working on this repository |

## License

MIT — see [LICENSE](LICENSE).

## Status

**2.4.1** — timeout module API, full summary/plan terminals, structured run
metadata, and shell completion routing. History: [CHANGELOG.md](CHANGELOG.md).
