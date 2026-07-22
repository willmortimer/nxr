# nxr documentation index

Product and architecture contract for `nxr`. Prefer these docs over inventing structure.

## Start here

| Doc | Purpose |
|---|---|
| [../README.md](../README.md) | Consumer-facing overview (install, commands, authoring) |
| [README.md](README.md) | Longer product narrative and design principles |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Developing and testing **this** repository |
| [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) | Locked decisions that must not drift |
| [TECH_STACK_AND_REPO_SHAPE.md](TECH_STACK_AND_REPO_SHAPE.md) | Languages, crates, and repository layout |
| [RELEASE.md](RELEASE.md) | Tag releases, artifacts, checksums, and SBOM |
| [ROADMAP.md](ROADMAP.md) | Shipped releases and active 2.5 → 3.1 plan |
| [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) | Post-2.5 expansion: contexts, secrets, HM, processes |
| [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) | Deferred control-plane ideas (not scheduled) |

## Core specs

| Doc | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | System architecture and execution model |
| [DESIGN.md](DESIGN.md) | Principles, tradeoffs, and semantic decisions |
| [FEATURES.md](FEATURES.md) | Feature set by capability area |
| [CLI_CONTRACT.md](CLI_CONTRACT.md) | Command surface and behavioral contract |
| [CLI_REFERENCE.md](CLI_REFERENCE.md) | Quick CLI index (`--help` companion) |
| [COMPATIBILITY.md](COMPATIBILITY.md) | Supported platforms and Nix expectations |
| [PERFORMANCE.md](PERFORMANCE.md) | V1 discovery/completion baselines |
| [TELEMETRY.md](TELEMETRY.md) | Telemetry decision (V1 default: none) |
| [SECURITY_REVIEW_V1.md](SECURITY_REVIEW_V1.md) | V1.0 security review vs ARCHITECTURE §8 |
| [APP_AUTHORING.md](APP_AUTHORING.md) | Conventions for robust flake apps |
| [TASKS.md](TASKS.md) | Declaring tasks and the `nxr.<system>` discovery attr |
| [MONOREPO_VIEWS.md](MONOREPO_VIEWS.md) | Category/namespace filters; optional non-authoritative projects file |
| [MIGRATE_FROM_MISE_JUST.md](MIGRATE_FROM_MISE_JUST.md) | How-to: move from mise/just/aliases to flake apps |
| [DEV_ENV_INTEGRATION.md](DEV_ENV_INTEGRATION.md) | Dev shells, direnv, DevPod, containers |
| [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) | Execution contexts, secret delivery, Home Manager, processes |
| [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md) | Adjacent-tool inheritance and boundaries |
| [ADAPTERS.md](ADAPTERS.md) | Read-only ecosystem graph adapter boundary (non-authority) |

## Architecture decisions

| Doc | Purpose |
|---|---|
| [adr/README.md](adr/README.md) | ADR index (Accepted / Proposed / Deferred) |
| [adr/template.md](adr/template.md) | Required ADR structure |

## Locked decisions (summary)

From [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md):

1. `nxr` is a Nix-native command / workflow / execution-context runner — not a package manager, secret store, or shell constructor.
2. Canonical leaf operation: `apps.<system>.<name>`; V1 `nxr <app>` ≈ `nix run .#<app> -- …`.
3. Discover flake root upward; preserve invocation CWD; inherit caller environment by default.
4. After the app name, arguments belong to the app; one `--` is removed; no shell evaluation.
5. No mandatory `nxr.toml` / YAML / JSON project file in V1.
6. Dev shells are optional interactive environments; apps are not auto-executed inside them.
7. V2 tasks coordinate apps; they do not replace apps.
8. Human and versioned machine output are both first-class.
9. Projects using `nxr` remain operable through standard Nix commands.
10. Layer ownership: flakes / direnv / devenv / SOPS / Home Manager / nxr each keep their boundary ([EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md)).
11. Secret values never appear in plans, events, or public metadata; execution-affecting schema fields must not be silently ignored.
