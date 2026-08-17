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
- NixPlane is a sibling fabric, not an NXR subsystem. NXR never stores fleet Profile Assignments ([docs/NIXPLANE.md](docs/NIXPLANE.md), [ADR-0174](docs/adr/0174-nixplane-fleet-state-boundary.md)).

Accepted foundational ADRs are listed in [docs/adr/README.md](docs/adr/README.md)
(including audit absorb ADR-0143–0150).
Active roadmap: [docs/ROADMAP.md](docs/ROADMAP.md) (shipped through 3.5.x;
next V4+ / [docs/vision/V4_EXECUTION_PROTOCOL.md](docs/vision/V4_EXECUTION_PROTOCOL.md)).

## Working agreements

- Prefer the stack and repo shape in [docs/TECH_STACK_AND_REPO_SHAPE.md](docs/TECH_STACK_AND_REPO_SHAPE.md).
- Do not widen scope past stated non-goals in [docs/README.md](docs/README.md).
- No secrets in logs, tests, or fixtures; follow [SECURITY.md](SECURITY.md) and docs security guidance.
- Ask before changing public API/CLI vocabulary fixed in [docs/CLI_CONTRACT.md](docs/CLI_CONTRACT.md).
- Prefer **`nxr task …`** for this repo’s quality/release graphs; flake apps
  (`nix run .#…`) are escape hatches / bootstrap when `nxr` is not on PATH yet.

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

## Project quality / release (nxr task DAG)

Prefer these over ad-hoc cargo or one-off `nix run` rituals:

```text
nxr task ci          # host: fmt-check → lint → test → deny → cli-ref (+ host stamp)
nxr task ci-linux    # Linux OS parity via OrbStack/Docker (native on Linux)
nxr task release     # dependsOn [ci, ci-linux], then tag helper (dry-run / --execute)
```

Escape hatches (same leaves, hermetic env wrappers):

- `nix build .#nxr`
- `nix run .#ci-gate` / `.#ci-gate-linux` / `.#release`
- `nix run .#fmt` / `.#lint` / `.#test` / `.#deny` / `.#cli-ref` / `.#cli-ref-gen`

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) and [docs/RELEASE.md](docs/RELEASE.md).
