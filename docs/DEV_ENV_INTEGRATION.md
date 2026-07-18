# Development Environment Integration

## 1. Overview

`nxr` is designed to fit into existing Nix development workflows rather than replace them.

```text
flake apps       executable project operations
dev shell        interactive development environment
direnv           automatic shell activation
DevPod           remote workspace placement
devcontainer     editor/container workspace contract
nxr              operation discovery and invocation
```

## 2. Development shells

### Outside a shell

```bash
nxr test
```

should work when the app is self-contained.

### Inside a shell

```bash
nix develop
nxr test
```

the app also inherits environment variables from the shell.

### Named shells

A project may expose:

```text
devShells.default
devShells.backend
devShells.frontend
devShells.ci
```

V2 supports:

```bash
nxr --shell backend test
```

This is an explicit execution policy, not an automatic assumption.

## 3. direnv and nix-direnv

Typical `.envrc`:

```bash
use flake
```

The development shell becomes active when entering the directory.

That shell may provide:

- `nxr`;
- language servers;
- editor tools;
- project environment variables;
- local service configuration;
- shell completion integration.

The process relationship is:

```text
direnv-loaded shell
  └── nxr
       └── nix run .#test
            └── app executable
```

## 4. Automatic completion through the dev shell

V2 should provide a Nix integration package containing shell snippets:

```text
share/nxr/shell/nxr.bash
share/nxr/shell/nxr.zsh
share/nxr/shell/nxr.fish
```

A flake-parts module can add a shell hook:

```nix
nxr.shellIntegration = {
  enable = true;
  shells = [ "bash" "zsh" "fish" ];
};
```

Conceptual generated hook:

```sh
if [ -n "${ZSH_VERSION:-}" ]; then
  source "${nxr}/share/nxr/shell/nxr.zsh"
elif [ -n "${BASH_VERSION:-}" ]; then
  source "${nxr}/share/nxr/shell/nxr.bash"
fi
```

Fish integration may rely on its native environment-loading mechanism or a generated hook compatible with the selected direnv integration.

Requirements:

- session-local only;
- idempotent;
- fast;
- no writes to dotfiles;
- no global completion mutation;
- safe when a shell is nested;
- supports completion cache invalidation.

## 5. Completion cache

Dynamic completion cannot perform a slow full Nix evaluation on every keypress.

Cache key inputs may include:

- canonical flake root;
- current system;
- `flake.nix` metadata;
- `flake.lock` metadata;
- selected Nix executable/version;
- optional nxr schema version.

Cache policy:

- short stale-while-revalidate window;
- immediate invalidation after explicit `nxr refresh`;
- safe fallback to reserved commands on timeout;
- no diagnostics printed into shell completion protocol.

## 6. DevPod

DevPod decides where the workspace runs.

Inside the workspace, `nxr` behaves normally if:

- Nix is installed;
- the repository is present;
- the store/daemon configuration is functional;
- required credentials and services are available.

Example `devcontainer.json` concept:

```json
{
  "name": "nxr project",
  "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
  "features": {
    "ghcr.io/devcontainers/features/nix:1": {}
  },
  "postCreateCommand": "nix develop --command true"
}
```

The project flake remains authoritative.

## 7. Devcontainers

A devcontainer can be treated as the transport and editor layer.

Recommended responsibilities:

### devcontainer

- base OS;
- mounts;
- ports;
- editor extensions;
- user identity;
- Nix installation;
- daemon permissions.

### flake

- toolchain;
- apps;
- checks;
- development shell;
- project commands.

Avoid separately pinning the same language toolchain in both the devcontainer image and the flake unless bootstrapping requires it.

## 8. CI

V1 CI use:

```bash
nxr test
nxr lint
```

or native:

```bash
nix run .#test
nix run .#lint
```

Hermetic validation:

```bash
nix flake check
```

V2 graph use:

```bash
nxr task ci --output plain
```

CI mode should automatically avoid interactive selectors and adapt output when no TTY is present.

## 9. Remote development hosts

Because the operation interface lives in the flake, a remote SSH or VM workspace needs only:

- compatible Nix;
- project checkout or flake reference;
- required credentials;
- optional binary cache access.

The same command surface remains:

```bash
nxr test
nxr dev
```

## 10. Environment policies

### inherit

Default behavior. Use caller environment.

### clean

Start with a documented allowlist and explicit additions.

```bash
nxr --clean-env --keep-env HOME test
```

### shell

Execute through a selected development shell.

```bash
nxr --shell backend test
```

### explicit

V2 task metadata specifies exact variables:

```nix
environment = {
  policy = "explicit";
  inherit = [ "HOME" "SSH_AUTH_SOCK" ];
  set.CI = "true";
};
```

Secrets should be referenced by variable name, never serialized into public plans.

## 11. Shell nesting

Executing:

```bash
nix develop -c nix run .#test
```

may create redundant environment layers.

`nxr` should:

- avoid entering a shell unless requested;
- detect when the selected dev shell is already active where feasible;
- offer `--shell always` for forced re-entry;
- offer `--shell auto` for best-effort detection;
- document that exact shell identity detection is imperfect without an integration marker.

The shell module can set:

```text
NXR_DEV_SHELL=<name>
NXR_FLAKE_ROOT=<path>
```

for reliable detection within integrated shells.

## 12. Local services

A development shell may set service endpoints but should not necessarily own service lifecycles.

Options:

- app-native service launcher;
- process-compose app;
- V2 parallel task group;
- external container orchestration;
- DevPod provider resources.

`nxr` should coordinate processes only when explicitly represented as a task or app.
