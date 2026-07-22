# Agent guide

## Source of truth

Product and architecture docs live in `docs/`. Start at `docs/INDEX.md`.

## Locked decisions

Do not contradict [docs/CONTRACT_SUMMARY.md](docs/CONTRACT_SUMMARY.md). In short:

- Flake apps (`apps.<system>.<name>`) are the canonical leaf operations.
- V1 does not require `nxr.toml`, YAML, or another task manifest.
- Nix owns toolchain and runtime pinning; `nxr` does not.
- Discover flake root upward; preserve invocation CWD; inherit caller env by default.
- After the app name, arguments belong to the app; strip one `--`; never shell-evaluate.
- Dev shells are complementary; apps are not auto-run inside them.
- V2 tasks coordinate apps; they do not replace them.
- Preserve direct `nix run` compatibility as an escape hatch.
- Version machine-readable schemas; sanitize untrusted metadata for terminals.
- nxr is an execution-context layer—not a replacement for direnv, devenv, Home Manager, or secret stores ([docs/EXECUTION_CONTEXT.md](docs/EXECUTION_CONTEXT.md)).
- Secret values never appear in plans/events; execution-affecting schema fields must not be silently ignored (schema v2).

Accepted foundational ADRs are listed in [docs/adr/README.md](docs/adr/README.md).
Active roadmap: [docs/ROADMAP.md](docs/ROADMAP.md) (2.5 → 3.1).

## Working agreements

- Prefer the stack and repo shape in [docs/TECH_STACK_AND_REPO_SHAPE.md](docs/TECH_STACK_AND_REPO_SHAPE.md).
- Do not widen scope past stated non-goals in [docs/README.md](docs/README.md).
- No secrets in logs, tests, or fixtures; follow [SECURITY.md](SECURITY.md) and docs security guidance.
- Ask before changing public API/CLI vocabulary fixed in [docs/CLI_CONTRACT.md](docs/CLI_CONTRACT.md).

## Layout

```text
crates/          Rust workspace (nxr-cli, nxr-core, nxr-nix, …)
nix/             Nix library, flake-parts modules, packaging
shell/           Bash/Zsh/Fish completion assets
schemas/         Versioned JSON schemas
fixtures/        Fixture flakes for integration tests (see fixtures/README.md)
tests/           CLI / Nix / process / compatibility tests
xtask/           Repo maintenance binary
docs/            Design contract and ADRs
```

## Project flake apps

Prefer these over ad-hoc cargo invocations in docs/CI:

- `nix build .#nxr`
- `nix run .#fmt` / `.#lint` / `.#test` / `.#deny`
