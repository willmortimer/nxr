# Contract Summary

This file records the shortest form of the decisions that must not drift.

## Product identity

`nxr` is a Nix-native command, workflow, and **execution-context** runner for
standard flake outputs.

It does not pin runtimes, replace Nix, own secret storage, construct development
shells, manage system activation, or require another project task manifest.

Expanded design: [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).

## Layer ownership

| Layer | Owns |
|---|---|
| Nix flakes | Packages, apps, checks, development shells, configurations, artifacts |
| direnv / nix-direnv | Automatic shell activation and cached shell environments |
| devenv / numtide/devshell | Optional richer development-environment authoring |
| SOPS / sops-nix / SecretSpec | Secret encryption, storage, and provisioning |
| Home Manager | User-level installation, global configuration, shell hooks, trust policy |
| **nxr** | Target discovery, execution contexts, DAGs, environment policy, runtime secret delivery, process supervision |

## Ecosystem synthesis

The intended product combines:

- just and mission-control command ergonomics;
- mise/devenv task and process orchestration;
- direnv-activated development shells and completion;
- monorepo affected analysis and CI execution;
- hermetic and remote action concepts from large build systems;

while preserving Nix as the owner of tools, environments, packages, apps, checks, stores, and native remote builds.

## Canonical operation

A standard app:

```text
apps.<system>.<name>
```

is the canonical leaf operation.

## V1 invocation

```bash
nxr <app> [args...]
```

is equivalent in intent to:

```bash
nix run .#<app> -- [args...]
```

subject to explicitly selected environment and working-directory policy.

## Default working directory

Discover the flake root upward, but preserve the invocation directory.

## Default environment

Inherit the caller's environment.

Executables should be supplied by the app closure rather than accidentally inherited from the development shell.

## Argument handling

After the app name, arguments belong to the app.

One explicit `--` separator is removed.

No shell evaluation occurs.

Runner options must not move after the app/task name (for example,
`nxr test --shell backend` is rejected as a design; use `nxr --shell backend test`
or planned `nxr in backend test`).

## Exit behavior

For one app, preserve its exit status whenever possible.

Preserve interactive terminal and signal behavior.

## V1 configuration

No mandatory `nxr.toml`, YAML, or JSON project file.

## Development shell

The development shell is an optional interactive environment and shell-integration carrier.

An app is not automatically executed inside it.

## direnv

`use flake` may load a shell that includes `nxr` and session-local completion.

No global shell files are modified by the project flake alone.
User-level hooks may be installed via Home Manager (planned).

Do not decrypt project secrets automatically from `.envrc`. Use execution
contexts for secret-bearing process launches.

## DevPod and devcontainers

They choose where the workspace runs.

The flake remains authoritative for tools and operations.

## V2 task definition

Tasks are optional, versioned orchestration metadata.

Tasks coordinate apps; they do not replace apps.

## V2 process behavior

Parallel groups, watchers, and DAGs are fully supervised:

- signals propagate;
- children are cleaned up;
- output is attributable;
- exit policy is deterministic.

## Execution contexts and secrets (planned)

Named contexts may bind shell, environment policy, secret **references**, and
confirmation. Secret **values** are resolved at spawn via user/CI provider
bindings (Home Manager or local config)—never during flake evaluation, and never
serialized into plans or events.

Task document **schema v2** is required for execution-affecting fields
(`context`, secrets, inputs/outputs, dependency states). Unknown execution or
security metadata must be **rejected**, not silently ignored.

## Output

Human output and stable machine output are both first-class.

Structured schemas are versioned.

## Caching

Nix caches derivations and realizations.

Native tools cache incremental work.

`nxr` may cache discovery metadata, not silently invent a general task artifact cache.

## Compatibility guarantee

A project using `nxr` remains operable through standard Nix commands.
