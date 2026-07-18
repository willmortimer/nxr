# Migrate from mise, just, and shell aliases

How to replace common project-command runners with standard Nix flake apps and `nxr`, without a second task manifest.

## What stays the same

- **Leaf operations** are ordinary `apps.<system>.<name>` outputs.
- **`nix run .#<app>`** remains a first-class escape hatch.
- **Tool versions** stay pinned by the flake / Nix, not by `nxr`.

## Mapping cheat sheet

| Before | After |
|---|---|
| `just test` / `mise run test` / `make test` | `apps.test` + `nxr test` (or `nix run .#test`) |
| `just --list` / `mise tasks` | `nxr` / `nxr list` |
| `alias t='cargo test'` in shell | Flake app that wraps `cargo test` with `runtimeInputs` |
| Project `mise.toml` tool pins | Nix packages in the app closure or `devShells` |
| Recipe that `cd`s to a subdir | Preserve invocation CWD (default) or use `--root` / `-C` |

## Recipe → flake app

`just` / Make recipes that shell out to tools should become `writeShellApplication` (or `lib.mkApp` from this repo):

```nix
apps.test = {
  type = "app";
  program = "${pkgs.writeShellApplication {
    name = "project-test";
    runtimeInputs = [ pkgs.cargo pkgs.cargo-nextest ];
    text = ''
      exec cargo nextest run "$@"
    '';
  }}/bin/project-test";
  meta.description = "Run the test suite";
};
```

Then:

```bash
nxr test
nxr test -- --nocapture
nix run .#test -- --nocapture   # same leaf, no nxr required
```

See [APP_AUTHORING.md](APP_AUTHORING.md) and [examples/mk-app](../examples/mk-app/).

## mise tools vs apps

| Concern | Owner |
|---|---|
| Compiler / linter / runtime pins | Nix (`packages`, app `runtimeInputs`, or `devShells`) |
| Interactive editor / direnv env | `devShells` + `direnv` (`use flake`) |
| “Run the test command” | Flake **app** |

Do not keep a second version pin in `mise.toml` for the same tool the flake already provides.

## Aliases

Replace:

```bash
alias lint='cargo clippy -- -D warnings'
```

with an `apps.lint` that embeds `clippy` in `runtimeInputs`, so CI and colleagues get the same closure.

## Day-one workflow after migration

```bash
nxr                  # list apps
nxr doctor           # static checks (non-destructive)
nxr test             # run an app
nxr plan test --json # inspect the exact nix run argv
nxr completion zsh   # optional shell integration
```

## What not to migrate into nxr

- Package management / runtime installs
- Deployment platforms
- Opaque task graphs that hide flake apps (V2 tasks are optional orchestration *around* apps, not a replacement)

## Related

- [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) — locked product boundaries
- [CLI_CONTRACT.md](CLI_CONTRACT.md) — command grammar
- [DEV_ENV_INTEGRATION.md](DEV_ENV_INTEGRATION.md) — shells, direnv, containers
