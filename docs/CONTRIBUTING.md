# Contributing to nxr

This page is for people working **on** the `nxr` repository. Consumers of `nxr`
in their own flakes should start at the [root README](../README.md) and the
[GitHub Wiki](https://github.com/willmortimer/nxr/wiki) (source markdown in
[`wiki/`](../wiki/); publish with [`scripts/publish-wiki.sh`](../scripts/publish-wiki.sh)).
Agents and design work stay in [`docs/`](INDEX.md).

## Develop in this repo

```bash
nix develop          # optional: project shell (cargo-nextest, cargo-deny, …)
nix build .#packages.$(nix eval --raw --impure --expr 'builtins.currentSystem').default
```

### Local ≡ CI quality gate

The product claim is one inspectable CI graph that is the same locally and on
GitHub Actions. In **this** repo that graph is authored as `nxr.tasks` and run
with `nxr task`:

| Command | What it proves | Pre-push? |
|---|---|---|
| `nxr task ci` | Host graph: fmt-check → lint → test → deny → **cli-ref** | Fast iteration |
| `nxr task ci-linux` | Same gate on **Linux + Determinate** (OrbStack/Docker; native when already on Linux) | **Yes — required before push to `main`** |
| `nxr task release` | `dependsOn = [ci, ci-linux]` then signed-tag helper | Before cutting a `v*` tag |
| `nix flake check -L` | Sandboxed derivation checks (includes hermetic `cli-ref`) | Yes (with Linux gate) |

`cli-ref` is **fail-closed** on Clap help drift (`docs/CLI_GENERATED.md`). Do not
auto-write in CI — regenerate when it fails:

```bash
cargo run -p xtask -- cli-ref    # or: nix run .#cli-ref-gen
```

```bash
nxr task ci              # Darwin/host quality graph (+ host release stamp)
nxr task ci-linux        # Linux OS parity (+ linux release stamp)
nxr task release         # both gates, then release dry-run
nxr task release -- --execute   # tag -s + push (requires gate stamps)
nix flake check -L
```

Quality flake apps clear `NXR_DEV_SHELL` and isolate git config so results match
a clean Actions runner. They deliberately **do not** pin `pkgs.nix` — discovery
capability negotiation must use the same ambient flakes-capable Nix as GHA
(Determinate). Pinning nixpkgs' `nix` flips cold discovery to `flake show` and
breaks call-budget ITs.

`ci-linux` prefers OrbStack machine **`nxr-ci-linux`** (ubuntu 24.04 +
Determinate Nix; created on first run). Falls back to Docker
(`nix/ci/Dockerfile.linux`). Default platform for Docker is native
(`linux/arm64` on Apple Silicon). For exact GHA arch:

```bash
NXR_CI_LINUX_PLATFORM=linux/amd64 NXR_CI_LINUX_BACKEND=docker nxr task ci-linux
```

Escape hatches when `nxr` is not on PATH yet: `nix run .#ci-gate`,
`.#ci-gate-linux`, `.#release` (same underlying apps).

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
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Do not treat host Cargo as release-blocking; use `nxr task ci` / `ci-linux`.

## Docs and ADRs

- Start at [INDEX.md](INDEX.md).
- Locked decisions: [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md).
- New design choices: add an ADR under `docs/adr/` (see [adr/README.md](adr/README.md)).

## Pull requests

- Keep diffs focused; update docs/tests with behavior changes.
- Run `nxr task ci` and `nxr task ci-linux` before pushing to `main`.
- No secrets in fixtures, logs, or commit messages.
