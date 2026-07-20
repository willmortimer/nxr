# Contributing to nxr

This page is for people working **on** the `nxr` repository. Consumers of `nxr` in their own flakes should start at the [root README](../README.md).

## Develop in this repo

```bash
nix develop          # optional: project shell
nix build .#nxr      # package the CLI
```

Quality apps (same ones CI runs):

```bash
nix run .#fmt        # rustfmt (add -- --check in CI)
nix run .#lint       # clippy -D warnings
nix run .#test       # cargo nextest
nix run .#deny       # cargo-deny
```

Or from a Cargo checkout:

```bash
cargo test -p nxr-cli
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
cargo run -p nxr-cli -- --flake fixtures/task-dag task ci -j 2 --dry-run
```

## Fixtures

Integration fixtures live under [`fixtures/`](../fixtures/README.md) (`basic-apps`, `task-dag`, `parallel-group`, `named-dev-shells`, `shell-integration`, …). Prefer them for CLI and discovery smoke tests instead of inventing one-off flakes.

## Demo GIF

The root README embeds a terminal GIF. How to regenerate it: [demo/README.md](demo/README.md).

## Docs map (maintainers)

| Doc | Purpose |
|---|---|
| [INDEX.md](INDEX.md) | Full documentation map |
| [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) | Locked product decisions |
| [ROADMAP.md](ROADMAP.md) | V1 → V3 delivery plan |
| [COMPATIBILITY.md](COMPATIBILITY.md) | Schema freeze, platforms, extension points |
| [ARCHITECTURE.md](ARCHITECTURE.md) | System design |
| [TECH_STACK_AND_REPO_SHAPE.md](TECH_STACK_AND_REPO_SHAPE.md) | Crates and layout |
| [CHANGELOG.md](../CHANGELOG.md) | Release history |

## Status

Workspace and Nix package are **2.1.0**. Do not push or tag from agent sessions unless a maintainer explicitly asks. A Ratatui-style dashboard remains long-term (roadmap Phase 35); do not add a TUI crate without an explicit decision.
