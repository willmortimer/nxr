# Contract Summary

This file records the shortest form of the decisions that must not drift.

## Product identity

`nxr` is an ergonomic runner for Nix flake apps.

It does not pin runtimes, replace Nix, or require another project task manifest.


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

No global shell files are modified.

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

## Output

Human output and stable machine output are both first-class.

Structured schemas are versioned.

## Caching

Nix caches derivations and realizations.

Native tools cache incremental work.

`nxr` may cache discovery metadata, not silently invent a general task artifact cache.

## Compatibility guarantee

A project using `nxr` remains operable through standard Nix commands.
