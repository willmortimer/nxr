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
    --refresh              Ignore nxr discovery cache
    --shell <NAME>         Execute through named dev shell
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

Not in this release (deferred): `--offline`.

Inline `flake#app` works on bare/`run`/`plan`/`doctor` targets (for example `nxr fixtures/basic-apps#hello`). Combining `--flake` with an inline `flake#app` is a usage error.

## Commands

| Command | Purpose |
|---|---|
| `nxr` | List apps (same as `nxr list`) |
| `nxr list` | List apps for the current system |
| `nxr <app> [args…]` | Run a flake app |
| `nxr <flake>#<app> [args…]` | Inline flake + app (like `nix run`) |
| `nxr run <app> [-- args…]` | Explicit run form |
| `nxr plan <app> [-- args…]` | Show execution plan |
| `nxr select` | Interactive fuzzy app picker |
| `nxr doctor [app]` | Diagnose environment and flake setup |
| `nxr doctor --all` | Extra non-destructive findings (descriptions, naming) |
| `nxr doctor --clean-env [app]` | Clean-environment validation |
| `nxr completion <shell>` | Emit Bash, Zsh, or Fish completion |
| `nxr inspect` | Overview of apps (+ tasks when present) |
| `nxr inspect app <name>` | Single app details |
| `nxr inspect task <name>` | Single task details |
| `nxr task <name> [args…]` | Run a task’s serial `dependsOn` chain |
| `nxr graph <name>` | Print task plan (text) |
| `nxr graph <name> --format mermaid` | Mermaid flowchart |
| `nxr watch <name> [--debounce <ms>]` | Watch flake root; kill+rerun app or task |

When `nxr watch` / name resolution finds both a task and an app with the same name, the **task** wins.

## Examples

```bash
nxr list --json
nxr test
nxr test -- --help
nxr fixtures/basic-apps#hello
nxr --flake ../other test
nxr plan test --json
nxr --shell default test
nxr doctor --clean-env test
nxr inspect
nxr task ci --dry-run
nxr graph ci --format mermaid
nxr watch test --debounce 300
nxr completion zsh > ~/.zfunc/_nxr

# Packaging / maintainers: generate man page
cargo run -p xtask -- man nxr.1
# or: nix build .#nxr  → share/man/man1/nxr.1

# In this repo, direnv materializes completions under .direnv/ (see .envrc).
# Zsh: source "$NXR_COMPLETION_HOOK" once, or add the precmd hook from
# docs/DEV_ENV_INTEGRATION.md.
```

Argument forwarding: one leading `--` is stripped; arguments are never shell-evaluated. See [CLI_CONTRACT.md](CLI_CONTRACT.md) §5.
