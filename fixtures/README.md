# Fixture flakes

These are small Nix flakes used to exercise `nxr` discovery and execution
(and, until `nxr` exists, plain `nix run`).

| Fixture | Purpose |
|---|---|
| [basic-apps](basic-apps/) | Common leaf apps: hello, echo-args, succeed, fail, pwd |
| [app-metadata](app-metadata/) | Apps with `meta.description` for listing UX |
| [nested-directory](nested-directory/) | Flake with a deep subdirectory for CWD / discovery tests |
| [broken-flake](broken-flake/) | Intentionally invalid flake for diagnostics |
| [named-dev-shells](named-dev-shells/) | `devShells.default` + `shell-marker` app for `--shell` |
| [shell-integration](shell-integration/) | `nxr.shellIntegration` adds `nxr` to `nix develop` |
| [task-dag](task-dag/) | Small task DAG (`fmt` → `test` → `ci`) via `nxr.<system>` |
| [task-working-directory](task-working-directory/) | Per-task `workingDirectory` tokens and relative paths |
| [parallel-group](parallel-group/) | Diamond DAG (`a` → `left`||`right` → `join`) for `-j` |
| [watch-project](watch-project/) | Placeholder for V2 watch mode |

## Try them (without nxr yet)

```bash
nix run ./fixtures/basic-apps#hello
nix run ./fixtures/basic-apps#echo-args -- one two
nix run ./fixtures/basic-apps#fail ; echo exit:$?
nix run ./fixtures/basic-apps#pwd
(cd fixtures/nested-directory/deep/down/here && nix run ../..#pwd)
nix flake show ./fixtures/app-metadata
nix eval --json ./fixtures/task-dag#nxr.aarch64-darwin
nix eval --json ./fixtures/task-dag#nxr.x86_64-linux
```

## Try them with nxr

From the repo root (requires `nix` on `PATH`):

```bash
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
cargo run -p nxr-cli -- --flake fixtures/basic-apps --json list
cargo run -p nxr-cli -- --flake fixtures/app-metadata list
cargo run -p nxr-cli -- --flake fixtures/basic-apps hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps run hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps plan hello --json
cargo run -p nxr-cli -- --flake fixtures/basic-apps echo-args -- alpha beta
cargo run -p nxr-cli -- --flake fixtures/basic-apps --dry-run fail
cargo run -p nxr-cli -- --flake fixtures/named-dev-shells --shell default shell-marker
cargo run -p nxr-cli -- --flake fixtures/named-dev-shells plan shell-marker --json
cargo run -p nxr-cli -- --flake fixtures/task-dag task ci
cargo run -p nxr-cli -- --flake fixtures/parallel-group task join -j 2
(cd fixtures/nested-directory/deep/down/here && cargo run -p nxr-cli -- list)
(cd fixtures/nested-directory/deep/down/here && cargo run -p nxr-cli -- pwd)
(cd fixtures/nested-directory/deep/down/here && cargo run -p nxr-cli -- --root pwd)
```

Integration tests in `crates/nxr-cli/tests/` exercise the same fixtures; they soft-skip when `nix` is missing locally. CI provides Nix.

## Project quality apps (this repo)

```bash
nix build .#nxr
nix run .#fmt -- --check
nix run .#lint
nix run .#test
nix run .#deny
```
