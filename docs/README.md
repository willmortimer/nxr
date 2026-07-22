# nxr

`nxr` is a zero-configuration command interface for standard Nix flake apps.

It treats a project's flake apps as its executable public interface:

```bash
nix run .#test
nix run .#lint
nix run .#dev
```

and makes them pleasant to discover and invoke:

```bash
nxr test
nxr lint -- --fix
nxr dev
```

The central idea is deliberately narrow:

> Flakes own inputs and executable operations. `nxr` makes those operations ergonomic without introducing another package manager, runtime pinning system, or mandatory task format.


## Ecosystem synthesis

The broader goal is to build the development workflow Nix already appears to imply:

> Combine the command ergonomics of just and mission-control, the task orchestration of mise and devenv, the automatic environment experience of direnv, and the monorepo/CI intelligence of systems such as Nx, moonrepo, Pants, and Bazel—while keeping flakes, apps, checks, development shells, and the Nix store as the canonical primitives.

`nxr` should feel native to an existing Nix repository. It must not ask the project to adopt a second runtime manager or rewrite every operation into a new proprietary task language.

The compatibility ladder is:

```text
nix run .#test
        ↓
nxr test
        ↓
nxr task ci
```

Each higher layer is optional, and each leaf operation remains an ordinary flake app.

Post-2.4 expansion (contexts, secrets, Home Manager, processes) is the committed
roadmap in [ROADMAP.md](ROADMAP.md) / [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).
Speculative control-plane steps beyond that remain ideas-only in
[ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md).

## Why nxr exists

Nix already provides:

- reproducible input locking through `flake.lock`;
- exact executable closures through packages and apps;
- remote-addressable execution through flake references;
- interactive environments through development shells;
- sandboxed, cacheable validation through checks;
- compatibility across local development, CI, remote workspaces, and containerized environments.

What it lacks is a polished project-command experience:

- listing available apps;
- showing useful descriptions;
- shell completion;
- fuzzy selection;
- consistent argument forwarding;
- working from nested directories;
- readable execution output;
- diagnostics for broken or accidentally impure app definitions;
- ergonomic composition for common developer workflows.

Projects therefore frequently add `mise`, `just`, Make, shell scripts, or bespoke wrappers even when Nix already owns the toolchain. `nxr` fills that UX gap while preserving ordinary `nix run` compatibility.

## Core model

```text
flake.nix
  ├── packages.<system>.*
  ├── apps.<system>.*          canonical executable operations
  ├── devShells.<system>.*     interactive workspace environments
  ├── checks.<system>.*        sandboxed/cacheable validation
  └── nxr metadata             optional, additive metadata and task graph

flake.lock                     pins all Nix inputs

.envrc                         optionally activates a development shell

DevPod / devcontainer          selects where the workspace executes

nxr                            discovers and invokes project operations
```

A project can use `nxr` without adopting any `nxr` Nix library. If it exports valid flake apps, `nxr` can run them.

## Design principles

1. **Standard flake apps first**  
   Leaf operations are ordinary `apps.<system>.<name>` outputs and remain directly runnable with `nix run`.

2. **No duplicate source of truth**  
   V1 does not require `nxr.toml`, YAML, or another task manifest.

3. **Nix owns resource pinning**  
   `nxr` does not install language runtimes, select tool versions, or compete with Nix.

4. **Development shells are complementary, not required**  
   Apps should normally carry their executable dependencies in their Nix closure. Development shells supply interactive tools, environment variables, local services, editor support, and shell integration.

5. **Portable project interface**  
   The same app should work locally, in CI, in DevPod, in a devcontainer, and from a remote flake reference.

6. **Human and machine interfaces are both first-class**  
   Pretty terminal output must coexist with stable JSON and event-stream modes.

7. **Optional power, not mandatory complexity**  
   V2 can add task graphs, groups, watches, and execution policies, while leaf apps remain standard flake apps.

8. **Native fit for existing Nix workflows**  
   Development shells, direnv, flake-parts, remote builders, checks, and normal `nix` commands remain first-class rather than being hidden behind the runner.

## Example flake

```nix
{
  description = "Example nxr project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
      ];

      perSystem = { pkgs, ... }:
        let
          test = pkgs.writeShellApplication {
            name = "project-test";
            runtimeInputs = [
              pkgs.cargo
              pkgs.cargo-nextest
            ];
            text = ''
              exec cargo nextest run "$@"
            '';
          };
        in {
          apps.test = {
            type = "app";
            program = "${test}/bin/project-test";
            meta.description = "Run the Rust test suite";
          };

          apps.default = {
            type = "app";
            program = "${test}/bin/project-test";
            meta.description = "Run the default project command";
          };

          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.cargo-nextest
              pkgs.rust-analyzer
              pkgs.nxr
            ];
          };
        };
    };
}
```

## User experience

```bash
$ nxr
Available apps

  test       Run the Rust test suite
  lint       Run static analysis
  fmt        Format the workspace
  dev        Start the local development environment
  deploy     Deploy the current revision

$ nxr test
✓ resolved .#test
▶ cargo nextest run
...

$ nxr test -- --nocapture
...

$ nxr --select
# interactive fuzzy picker
```

## Version strategy

### V1: excellent standard-app runner

V1 is intentionally small:

- discover standard flake apps;
- run local and remote app references;
- preserve argument boundaries correctly;
- find the flake root from nested directories;
- expose descriptions and machine-readable listings;
- support shell completion;
- provide readable diagnostics and output modes;
- validate app definitions;
- work inside or outside a development shell.

### V2: workflow orchestration

V2 adds optional workflow semantics:

- task DAGs;
- serial and parallel groups;
- watch mode and restart policies;
- stronger development-shell integration;
- shell integration automatically installed by the development shell;
- process-tree supervision;
- structured output multiplexing;
- richer task metadata;
- explicit working-directory and environment policies;
- interactive and non-interactive execution modes.

The task layer is additive. A V2 task graph points to standard apps or external commands; it does not invalidate the V1 model.


### V2.x: stabilization and ergonomics

After V2.0, minor releases focus on trustworthiness, flake UX, and monorepo ergonomics without introducing a second project graph or remote execution layer. See [ROADMAP.md](ROADMAP.md).

### Deferred: workspace control plane

Ideas for projects, affected analysis, action contracts, artifact caches, CI planning, remote workers, service fabric, and IDE protocols are preserved in [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md). They are not scheduled work. Nix derivations continue to use the Nix store, substituters, and remote builders; `nxr` does not replace those primitives.

## Non-goals

`nxr` is not:

- a replacement for Nix;
- a general package manager;
- another runtime version manager;
- a deployment platform;
- a container runtime;
- a remote build service;
- a Nix language replacement;
- a requirement for running project apps;
- an excuse to hide all project logic inside opaque runner configuration.

## Documentation map

Start at [INDEX.md](INDEX.md).

- [ARCHITECTURE.md](ARCHITECTURE.md) — system architecture and execution model
- [DESIGN.md](DESIGN.md) — principles, tradeoffs, and semantic decisions
- [FEATURES.md](FEATURES.md) — feature set by capability area
- [CLI_CONTRACT.md](CLI_CONTRACT.md) — command surface and behavioral contract
- [APP_AUTHORING.md](APP_AUTHORING.md) — conventions for robust flake apps
- [DEV_ENV_INTEGRATION.md](DEV_ENV_INTEGRATION.md) — dev shells, direnv, DevPod, and containers
- [TECH_STACK_AND_REPO_SHAPE.md](TECH_STACK_AND_REPO_SHAPE.md) — implementation stack and repository layout
- [ROADMAP.md](ROADMAP.md) — shipped V1–V2 and active 2.1–2.3 plan
- [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) — deferred V3 control-plane ideas (not scheduled)
- [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md) — which ideas to inherit from adjacent tools and which boundaries to preserve
- [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) — locked decisions that must not drift
- [adr/README.md](adr/README.md) — architecture decisions (Accepted / Proposed / Deferred)
- [adr/template.md](adr/template.md) — required structure for individual ADRs
