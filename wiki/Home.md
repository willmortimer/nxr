# nxr wiki

Ergonomic command plane for **standard Nix flake outputs**.

`nxr test` ≈ `nix run .#test`. Apps stay the leaves; `nix run` stays the escape
hatch. Tasks compose apps into inspectable DAGs that are the same locally and in
CI.

## Guides

| Page | Topic |
|---|---|
| [Install](Install) | Profile, shell, flake-parts module |
| [Tasks and DAGs](Tasks-and-DAGs) | `nxr task`, graphs, parameters, wizard branching |
| [Interactive TUI](Interactive-TUI) | `nxr ui`, `--output tui`, `nxr attach`, OSC 52 |
| [CI and Release](CI-and-Release) | Local ≡ CI, `nxr ci plan`, release gates |
| [Migrations](Migrations) | From mise / just |

## Contract (short)

- Flake apps (`apps.<system>.<name>`) are canonical leaf operations.
- V1 does not require `nxr.toml` or a second task manifest language.
- Nix owns toolchains; nxr does not.
- After the app name, arguments belong to the app.

Deep design docs and ADRs live in the repo under
[`docs/`](https://github.com/willmortimer/nxr/tree/main/docs) — start at
[INDEX.md](https://github.com/willmortimer/nxr/blob/main/docs/INDEX.md).

Landing page: [README](https://github.com/willmortimer/nxr#readme).
