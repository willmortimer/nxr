# CLI reference

Authoritative behavior and exit codes live in [CLI_CONTRACT.md](CLI_CONTRACT.md). This page is a quick index; when in doubt, use `--help`.

```bash
nxr --help
nxr <command> --help
```

## Global options

```text
-f, --flake <REF>     Select flake reference
-C, --cwd <PATH>      Set child working directory
    --root            Run child from flake root
    --dry-run         Print plan without execution
    --json            Emit JSON for data-returning commands
    --nix <PATH>      Override Nix executable
-s, --select          Open interactive app selector
    --refresh         Ignore nxr discovery cache
-q, --quiet           Suppress non-error nxr messages
-v, --verbose         Increase runner diagnostics
    --plain           Disable decorative terminal output
    --no-color        Disable runner color
    --color <WHEN>    auto|always|never
-h, --help            Show help
-V, --version         Show version
```

## Commands (V1)

| Command | Purpose |
|---|---|
| `nxr` | List apps (same as `nxr list`) |
| `nxr list` | List apps for the current system |
| `nxr <app> [args…]` | Run a flake app |
| `nxr run <app> [-- args…]` | Explicit run form |
| `nxr plan <app> [-- args…]` | Show execution plan |
| `nxr select` | Interactive fuzzy app picker |
| `nxr doctor [app]` | Diagnose environment and flake setup |
| `nxr doctor --clean-env [app]` | Clean-environment validation |
| `nxr completion <shell>` | Emit Bash, Zsh, or Fish completion |

Reserved for V2 (present but not implemented): `inspect`, `task`, `watch`, `graph`.

## Examples

```bash
nxr list --json
nxr test
nxr test -- --help
nxr --flake ../other test
nxr plan test --json
nxr doctor --clean-env test
nxr completion zsh > ~/.zfunc/_nxr
```

Argument forwarding: one leading `--` is stripped; arguments are never shell-evaluated. See [CLI_CONTRACT.md](CLI_CONTRACT.md) §5.
