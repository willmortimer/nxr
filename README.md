# nxr

Zero-configuration command interface for standard Nix flake apps.

Treat flake apps as the project's executable public interface:

```bash
nix run .#test
nxr test
```

`nxr` makes those operations discoverable and pleasant without adding another package manager, runtime pin tool, or mandatory task format.

## Docs

Start at **[docs/INDEX.md](docs/INDEX.md)**. Locked decisions live in [docs/CONTRACT_SUMMARY.md](docs/CONTRACT_SUMMARY.md).

## Develop

```bash
nix develop
```

## Project apps

These are the same operations CI runs:

```bash
nix build .#nxr          # package the CLI
nix run .#fmt            # rustfmt (add -- --check in CI)
nix run .#lint           # clippy -D warnings
nix run .#test           # cargo nextest
nix run .#deny           # cargo-deny
```

## How we test

1. **Repo quality apps** — `fmt` / `lint` / `test` / `deny` above (and `.github/workflows/ci.yml`).
2. **Fixture flakes** under [`fixtures/`](fixtures/README.md) — stand-ins for user projects with common task shapes (`hello`, `echo-args`, `fail`, `pwd`, metadata, nested dirs).

```bash
nix run ./fixtures/basic-apps#hello
nix run ./fixtures/basic-apps#echo-args -- one two
(cd fixtures/nested-directory/deep/down/here && nix run ../..#pwd)

# nxr (requires nix on PATH)
cargo run -p nxr-cli -- --flake fixtures/basic-apps list
cargo run -p nxr-cli -- --flake fixtures/basic-apps --json list
cargo run -p nxr-cli -- --flake fixtures/basic-apps hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps run hello
cargo run -p nxr-cli -- --flake fixtures/basic-apps plan hello --json
cargo run -p nxr-cli -- --flake fixtures/basic-apps --dry-run fail
(cd fixtures/nested-directory/deep/down/here && cargo run -p nxr-cli -- pwd)
```

## License

MIT — see [LICENSE](LICENSE).

## Status

**1.0.0** — V1 standard flake app runner (Phases 0–6). Discover, list, run, plan, select, completion, cache, doctor, man page, and app-authoring helpers. See [CHANGELOG.md](CHANGELOG.md).
