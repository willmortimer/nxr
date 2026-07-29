# Contributing to nxr

This page is for people working **on** the `nxr` repository. Consumers of `nxr` in their own flakes should start at the [root README](../README.md).

## Develop in this repo

```bash
nix develop          # optional: project shell (cargo-nextest, cargo-deny, …)
nix build .#packages.$(nix eval --raw --impure --expr 'builtins.currentSystem').default
```

### Local ≡ CI quality gate

Use the same Nix entrypoint GitHub Actions uses. Do **not** treat host
`cargo nextest` / `cargo clippy` as CI parity — those inherit shell pollution
(`NXR_DEV_SHELL`, git signing agents) and host toolchains.

```bash
nix run .#ci-gate    # packaged nxr + task ci (fmt-check → lint/test/deny)
nix flake check -L   # hermetic derivation checks (sandbox; skips Nix ITs)
```

`ci-gate` clears `NXR_DEV_SHELL` and isolates git config so results match a
clean Actions runner. Quality flake apps (`test`, `lint`, `deny`, …) do the same.
They deliberately **do not** pin `pkgs.nix` — discovery capability negotiation
must use the same ambient flakes-capable Nix as GHA (Determinate). Pinning
nixpkgs' `nix` flips cold discovery to `flake show` and breaks call-budget ITs.

Individual apps (still hermetic via nixpkgs toolchains):

```bash
nix run .#fmt        # rustfmt (add -- --check for CI-style)
nix run .#lint       # clippy -D warnings
nix run .#test       # cargo nextest
nix run .#deny       # cargo-deny
nxr ci plan --json   # provider-neutral CI plan export
```

Host Cargo is fine for fast iteration only:

```bash
cargo test -p nxr-cli
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
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

Workspace and Nix package are **2.3.0**. Do not push or tag from agent sessions unless a maintainer explicitly asks. A Ratatui-style dashboard remains long-term (roadmap Phase 35); do not add a TUI crate without an explicit decision.
