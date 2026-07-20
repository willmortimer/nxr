# CLI reference

Authoritative behavior and exit codes live in [CLI_CONTRACT.md](CLI_CONTRACT.md). This page is a quick index; when in doubt, use `--help`.

```bash
nxr --help
nxr <command> --help
```

## Global options

```text
-f, --flake <REF>          Select flake reference
-C, --cwd <PATH>           Set child working directory
    --root                 Run child from flake root
    --dry-run              Print plan without execution
    --json                 Emit JSON for data-returning commands
    --nix <PATH>           Override Nix executable
-s, --select               Open interactive app selector
    --refresh-discovery    Ignore nxr discovery cache
    --offline              Forward `--offline` to Nix when supported
    --accept-flake-config  Forward `--accept-flake-config` to Nix when supported
    --nix-option <KEY=VAL> Forward `--option KEY VAL` to Nix (repeatable)
    --nix-arg <ARG>        Forward arbitrary Nix argv fragments (repeatable)
    --shell <NAME>         Execute through named dev shell
    --shell-mode <MODE>    smart|always|never (default smart)
    --clean-env            Run with reduced inherited environment
    --keep-env <NAME>      Preserve variable in clean mode (repeatable)
    --set-env <KEY=VALUE>  Set or replace a variable (repeatable)
    --unset-env <NAME>     Remove a variable (repeatable)
-q, --quiet                Suppress non-error nxr messages
-v, --verbose              Increase runner diagnostics
    --plain                Disable decorative terminal output
    --log-format <FORMAT>  human|plain|json (runner stderr diagnostics)
    --no-color             Disable runner color
    --color <WHEN>         auto|always|never
-h, --help                 Show help
-V, --version              Show version
```

`--keep-env` / `--set-env` / `--unset-env` require `--clean-env`. Clean mode starts from the allowlist in `nxr_core::CLEAN_ENV_ALLOWLIST` (documented in [DEV_ENV_INTEGRATION.md](DEV_ENV_INTEGRATION.md) §10); `PATH` is not allowlisted so shell pollution is visible.

Not in this release (deferred): none. Use `--nix-arg` for other Nix globals (for example `--nix-arg --refresh`).

Inline `flake#app` works on bare/`run`/`plan`/`doctor` targets (for example `nxr fixtures/basic-apps#hello`). Combining `--flake` with an inline `flake#app` is a usage error.

## Commands

| Command | Purpose |
|---|---|
| `nxr` | List apps (same as `nxr list`) |
| `nxr list` | List apps (and tasks when present) for the current system |
| `nxr list apps\|checks\|packages\|shells\|tasks` | List one catalog (default without kind: apps + tasks) |
| `nxr list --category <name>` | Filter listed apps/tasks by category |
| `nxr list --namespace <name>` | Filter by project namespace (`nxr.projects.json`) |
| `nxr <app> [args…]` | Run a flake app |
| `nxr <flake>#<app> [args…]` | Inline flake + app (like `nix run`) |
| `nxr run <app> [-- args…]` | Explicit run form |
| `nxr build [name]` | `nix build` for `packages.<system>.<name>` (default package when omitted) |
| `nxr check [name]` | Build `checks.<system>.<name>`, or `nix flake check` when omitted |
| `nxr shell [name]` | Interactive `nix develop` for `devShells.<system>.<name>` (default when omitted) |
| `nxr plan <app\|task> [-- args…]` | Show app or task execution plan (apps win when both exist) |
| `nxr select` | Interactive fuzzy app picker |
| `nxr doctor [app]` | Diagnose environment and flake setup |
| `nxr doctor --all` | Extra non-destructive findings (descriptions, naming, cache) |
| `nxr doctor --clean-env [app]` | Clean-environment validation |
| `nxr explain <app\|task> [-- args…]` | Explain resolution and exact Nix invocation (apps win when both exist) |
| `nxr explain app <name> [-- args…]` | Explain a single app |
| `nxr explain task <name> [-- args…]` | Explain a task DAG node plans and dependency path |
| `nxr completion <shell>` | Emit Bash, Zsh, or Fish completion |
| `nxr cache clear` | Remove all discovery cache entries |
| `nxr cache status` | Show discovery cache path and size |
| `nxr affected [--base <ref>] [PATH…]` | Report apps and tasks likely affected by changed paths (`--json` for CI) |
| `nxr inspect` | Overview of apps (+ tasks when present) |
| `nxr inspect --category <name>` | Overview with apps/tasks filtered by category |
| `nxr inspect --namespace <name>` | Overview filtered by project namespace |
| `nxr inspect app <name>` | Single app details |
| `nxr inspect task <name>` | Single task details |
| `nxr task <name>… [args…]` | Run one or more task roots as a union DAG (shared deps run once); trailing args go to each **root** task app only |
| `nxr graph <name>` | Print task plan (text) |
| `nxr graph <name> --format dot` | Graphviz DOT digraph (does not invoke Graphviz) |
| `nxr graph <name> --format mermaid` | Mermaid flowchart |
| `nxr watch <name> [--debounce <ms>] [--include <glob>]… [--exclude <glob>]… [--clear]` | Watch flake root; kill+rerun app or task |
| `nxr run <app> --watch [--debounce <ms>]` | Alias into watch for a single app |
| `nxr task <name> --watch [--debounce <ms>]` | Alias into watch for a task (serial chain) |

When `nxr watch` / name resolution finds both a task and an app with the same name, the **task** wins.

`--include` restricts restarts to paths matching at least one glob; `--exclude` adds ignores on top of built-in skips (`.git`, `target`, `result*`, `/nix/store`). With no `--include`, any non-ignored path under the flake root can trigger a restart.

## Examples

```bash
nxr list --json
nxr list packages
nxr list shells --json
nxr build marker --dry-run
nxr check ok --dry-run
nxr shell backend --dry-run
nxr test
nxr test -- --help
nxr fixtures/basic-apps#hello
nxr --flake ../other test
nxr plan test --json
nxr plan ci --json
nxr --shell default test
nxr doctor --clean-env test
nxr explain hello --json
nxr explain task ci
nxr inspect
nxr task ci --dry-run
nxr task lint unit integration -j 8
nxr graph ci --format dot
nxr graph ci --format mermaid
nxr watch test --debounce 300
nxr watch dev --include 'src/**' --exclude 'src/generated/**' --clear
nxr run test --watch
nxr task dev --watch --debounce 500
nxr completion zsh > ~/.zfunc/_nxr

# Packaging / maintainers: generate man page
cargo run -p xtask -- man nxr.1
# or: nix build .#nxr  → share/man/man1/nxr.1

# In this repo, direnv materializes completions under .direnv/ (see .envrc).
# Zsh: source "$NXR_COMPLETION_HOOK" once, or add the precmd hook from
# docs/DEV_ENV_INTEGRATION.md.
```

Argument forwarding: one leading `--` is stripped; arguments are never shell-evaluated. See [CLI_CONTRACT.md](CLI_CONTRACT.md) §5. For `nxr task`, trailing args go to the root task app only; see [TASKS.md](TASKS.md).
