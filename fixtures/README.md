# Fixture flakes

These are small Nix flakes used to exercise `nxr` discovery and execution
(and, until `nxr` exists, plain `nix run`).

| Fixture | Purpose |
|---|---|
| [basic-apps](basic-apps/) | Common leaf apps: hello, echo-args, succeed, fail, pwd |
| [app-metadata](app-metadata/) | Apps with `meta.description` for listing UX |
| [nested-directory](nested-directory/) | Flake with a deep subdirectory for CWD / discovery tests |
| [broken-flake](broken-flake/) | Intentionally invalid flake for diagnostics |
| [named-dev-shells](named-dev-shells/) | Placeholder for named `devShells` (later) |
| [task-dag](task-dag/) | Placeholder for V2 task graphs |
| [parallel-group](parallel-group/) | Placeholder for V2 parallel groups |
| [watch-project](watch-project/) | Placeholder for V2 watch mode |

## Try them (without nxr yet)

```bash
nix run ./fixtures/basic-apps#hello
nix run ./fixtures/basic-apps#echo-args -- one two
nix run ./fixtures/basic-apps#fail ; echo exit:$?
nix run ./fixtures/basic-apps#pwd
(cd fixtures/nested-directory/deep/down/here && nix run ../..#pwd)
nix flake show ./fixtures/app-metadata
```

## Project quality apps (this repo)

```bash
nix build .#nxr
nix run .#fmt -- --check
nix run .#lint
nix run .#test
nix run .#deny
```
