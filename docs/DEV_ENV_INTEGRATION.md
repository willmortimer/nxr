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

Typical `.envrc` (this repository):

```bash
use flake
# …then materialize `nxr completion` scripts under `.direnv/` and export
# NXR_COMPLETION_HOOK for interactive zsh (see shell/direnv-zsh-hook.zsh).
```

The development shell becomes active when entering the directory.

That shell may provide:

- `nxr`;
- language servers;
- editor tools;
- project environment variables;
- local service configuration;
- shell completion integration.

### Zsh + direnv

direnv cannot inject shell functions into the parent shell. After
`eval "$(direnv hook zsh)"` in `~/.zshrc`, add:

```zsh
# Load project-local completion hooks exported by .envrc (nxr, etc.).
_direnv_completion_hooks() {
  [[ -n ${NXR_COMPLETION_HOOK:-} && -f $NXR_COMPLETION_HOOK ]] || return 0
  [[ ${NXR_COMPLETION_HOOK_LOADED:-} == $NXR_COMPLETION_HOOK ]] && return 0
  # shellcheck disable=SC1090
  source "$NXR_COMPLETION_HOOK"
  NXR_COMPLETION_HOOK_LOADED=$NXR_COMPLETION_HOOK
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd _direnv_completion_hooks
```

Or source once per session: `source "$NXR_COMPLETION_HOOK"`.

The process relationship is:

```text
direnv-loaded shell
  └── nxr
       └── nix run .#test
            └── app executable
```

## 4. Automatic completion through the dev shell

The `nxr` flake-parts module can install session-local shell integration into
selected dev shells. Snippets ship with the `nxr` package:

```text
share/nxr/shell/nxr.bash
share/nxr/shell/nxr.zsh
share/nxr/shell/nxr.fish
share/nxr/shell/integrate.{bash,zsh,fish}
share/nxr/shell/direnv-zsh-hook.zsh
```

Enable integration in your flake:

```nix
imports = [ nxr.flakeModules.default ];

perSystem = { system, ... }: {
  packages.nxr = nxr.packages.${system}.nxr;

  nxr.shellIntegration = {
    enable = true;
    devShells = [ "default" "backend" ];
    # package = nxr.packages.${system}.nxr;  # optional when packages.nxr exists
  };
};
```

When `enable` is true, each listed `devShell` receives:

- the `nxr` package on `PATH`;
- `NXR_SHELL_INTEGRATION=1` so nested `nix develop` does not double-source hooks;
- `NXR_DEV_SHELL`, `NXR_COMPLETION_DIR`, `XDG_DATA_DIRS`, and `FPATH` exports for
  completion discovery;
- an interactive Bash/Zsh hook that loads package completions (Fish inherits
  vendor completions via `XDG_DATA_DIRS`).

No global dotfiles are written. Prompt indicators are not enabled by default.

Conceptual generated hook (Bash/Zsh):

```sh
if [ -z "${NXR_SHELL_INTEGRATION:-}" ]; then
  export NXR_SHELL_INTEGRATION=1
  export NXR_DEV_SHELL=default
  export NXR_PACKAGE="${nxr}"
  export NXR_COMPLETION_DIR="${nxr}/share"
  export XDG_DATA_DIRS="${nxr}/share${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
  export FPATH="${nxr}/share/zsh/site-functions${FPATH:+:$FPATH}"
  # interactive: source integrate.bash / integrate.zsh from share/nxr/shell/
fi
```

Fish integration relies on vendor completions under `share/fish/vendor_completions.d/`
plus the optional `integrate.fish` / `nxr.fish` snippets when sourced manually.

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

Start with the documented allowlist (`CLEAN_ENV_ALLOWLIST` in `nxr-core`: `HOME`, `USER`, `LOGNAME`, `TMPDIR` / `TMP` / `TEMP`, `TERM`, `COLORTERM`, `LANG` / `LC_*`, display/socket vars, XDG dirs, Nix/SSL CA vars). **`PATH` is not allowlisted.**

```bash
nxr --clean-env --keep-env HOME --set-env CI=1 test
nxr plan --clean-env test --json   # environment_policy is a clean object
```

`--keep-env`, `--set-env`, and `--unset-env` require `--clean-env`.

### shell

Execute through a selected development shell via `nix develop <flake>#<name> -c <nix> run …`.

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
