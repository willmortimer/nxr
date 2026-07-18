# nxr documentation index

Product and architecture contract for `nxr`. Prefer these docs over inventing structure.

## Start here

| Doc | Purpose |
|---|---|
| [README.md](README.md) | Product overview and design principles |
| [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) | Locked decisions that must not drift |
| [TECH_STACK_AND_REPO_SHAPE.md](TECH_STACK_AND_REPO_SHAPE.md) | Languages, crates, and repository layout |
| [ROADMAP.md](ROADMAP.md) | V1–V3.5 delivery plan |

## Core specs

| Doc | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | System architecture and execution model |
| [DESIGN.md](DESIGN.md) | Principles, tradeoffs, and semantic decisions |
| [FEATURES.md](FEATURES.md) | Feature set by capability area |
| [CLI_CONTRACT.md](CLI_CONTRACT.md) | Command surface and behavioral contract |
| [APP_AUTHORING.md](APP_AUTHORING.md) | Conventions for robust flake apps |
| [MIGRATE_FROM_MISE_JUST.md](MIGRATE_FROM_MISE_JUST.md) | How-to: move from mise/just/aliases to flake apps |
| [DEV_ENV_INTEGRATION.md](DEV_ENV_INTEGRATION.md) | Dev shells, direnv, DevPod, containers |
| [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md) | Adjacent-tool inheritance and boundaries |

## Architecture decisions

| Doc | Purpose |
|---|---|
| [adr/README.md](adr/README.md) | ADR index (Accepted / Proposed / Deferred) |
| [adr/template.md](adr/template.md) | Required ADR structure |

## Locked decisions (summary)

From [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md):

1. `nxr` is an ergonomic runner for Nix flake apps — not a package manager or runtime pin tool.
2. Canonical leaf operation: `apps.<system>.<name>`; V1 `nxr <app>` ≈ `nix run .#<app> -- …`.
3. Discover flake root upward; preserve invocation CWD; inherit caller environment by default.
4. After the app name, arguments belong to the app; one `--` is removed; no shell evaluation.
5. No mandatory `nxr.toml` / YAML / JSON project file in V1.
6. Dev shells are optional interactive environments; apps are not auto-executed inside them.
7. V2 tasks coordinate apps; they do not replace apps.
8. Human and versioned machine output are both first-class.
9. Projects using `nxr` remain operable through standard Nix commands.
