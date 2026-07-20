# Features

## 1. V1 feature set

### 1.1 Zero-configuration app discovery

`nxr` discovers:

```text
apps.<current-system>.*
```

from the selected flake.

It displays:

- app name;
- description;
- default marker;
- flake reference;
- current system;
- optional metadata.

Commands:

```bash
nxr
nxr list
nxr list --json
nxr list apps|checks|packages|shells|tasks
```

### 1.1.1 Native flake outputs

`nxr` also catalogs and invokes standard non-app flake outputs via the same
`nix flake show` discovery path used for apps:

```bash
nxr list packages
nxr list checks
nxr list shells
nxr build [name]      # nix build .#packages.<system>.<name>
nxr check [name]      # nix build .#checks.<system>.<name>  (or nix flake check)
nxr shell [name]      # nix develop .#<name>
```

These commands are thin ergonomics over Nix. They do not redefine checks as
nxr tasks, and `nix build` / `nix flake check` / `nix develop` remain escape
hatches.

### 1.2 Ergonomic execution

```bash
nxr test
nxr run test
nxr test -- --nocapture
nxr --flake ../other-project test
nxr --flake github:owner/repo test
```

Execution preserves:

- argument order;
- quoting boundaries;
- stdin;
- terminal control;
- exit code;
- signals;
- current working directory.

### 1.3 Flake-root discovery

From any nested directory:

```bash
nxr test
```

walks upward until a `flake.nix` is found.

Options:

```bash
nxr --root test
nxr --cwd ./crates/api test
nxr --flake-root /path/to/repo test
```

### 1.4 Fuzzy picker

```bash
nxr --select
nxr select
```

The picker shows names and descriptions and returns one selected app.

The core binary should not require a heavy TUI framework merely for selection. A lightweight terminal selector is enough for V1.

### 1.5 Shell completion

Generated completion for:

- Bash;
- Zsh;
- Fish.

Completion candidates are dynamic per flake and include descriptions.

```bash
nxr completion bash
nxr completion zsh
nxr completion fish
```

### 1.6 Output modes

```bash
nxr test
nxr --quiet test
nxr --verbose test
nxr --plain test
nxr list --json
nxr --log-format json test
```

V1 defaults to transparent child output.

### 1.7 Diagnostics

Useful errors for:

- no flake found;
- no current-system apps;
- app not found;
- unsupported Nix version;
- flake evaluation failure;
- invalid app program;
- remote reference failure;
- malformed metadata;
- unexpected child termination.

Suggestions should use fuzzy matching:

```text
app "tset" not found

Did you mean:
  test
```

### 1.8 Doctor

```bash
nxr doctor
nxr doctor test
nxr doctor --all
nxr doctor --clean-env
```

Checks may include:

- app discovery succeeds;
- selected app resolves;
- program is a valid executable;
- app runs with `--help` only when explicitly configured;
- app does not accidentally depend on development-shell `PATH`;
- descriptions are present;
- names follow recommended conventions;
- optional metadata matches supported schema.

Doctor must avoid executing destructive apps by default.

### 1.9 Plan and dry-run

```bash
nxr plan test
nxr plan test --json
nxr --dry-run test -- --nocapture
```

The plan includes:

- selected flake;
- app attr path;
- current system;
- invocation directory;
- execution directory;
- environment policy;
- exact Nix argument vector;
- forwarded app arguments.

### 1.10 Remote flakes

```bash
nxr --flake github:owner/project test
nxr github:owner/project#test
```

Remote execution should be explicit in output and compatible with normal Nix trust behavior.

### 1.11 Nix authoring helpers

An optional Nix library provides:

- `mkApp`;
- `mkScriptApp`;
- `mkPackageApp`;
- metadata helpers;
- shared app/dev-shell tool lists;
- flake-parts integration.

The helper output remains a standard app.

## 2. V2 feature set

### 2.1 Versioned task graph

Optional task metadata introduces:

- dependencies (`dependsOn` DAG);
- serial groups;
- parallel execution via `-j` (parallel group sugar deferred);
- failure policy;
- concurrency limits;
- environment policy;
- working directory;
- app references;
- descriptions and categories.

Commands:

```bash
nxr task ci
nxr plan ci
nxr graph ci
```

`nxr <name>` may resolve both tasks and apps, with explicit conflict rules.

### 2.2 DAG visualization

```bash
nxr graph ci
nxr graph ci --format dot
nxr graph ci --format mermaid
nxr graph ci --json
```

Example:

```text
fmt-check ─┐
lint ──────┼──> package ──> deploy
test ──────┘
```

### 2.3 Parallel execution

V2.0 models parallelism with a `dependsOn` DAG and `nxr task -j N` (job limit on the ready queue). Tasks with no dependency edge between them may run concurrently when `-j` allows.

The `parallel = [ … ]` sugar (grouped siblings with shared fail-fast and labels) is **deferred** past V2.0.

Capabilities (V2.0):

- `dependsOn` DAG with deterministic scheduling;
- `-j N` concurrency limit on ready tasks;
- process-group cleanup;
- fail-fast or keep-running failure policy;
- summarized exit status.

Deferred (post–V2.0):

- `parallel = [ … ]` group sugar;
- labeled/colored parallel group output.

### 2.4 Watch mode

```bash
nxr watch test
nxr watch dev
nxr test --watch
```

Capabilities:

- native filesystem notifications;
- debounce configuration;
- include and exclude globs;
- restart or rerun modes;
- clear-screen option;
- keep previous output option;
- dependent subgraph invalidation;
- graceful process replacement.

### 2.5 Development-shell execution

V2 supports explicit execution inside a named shell:

```bash
nxr --shell default test
nxr --shell backend task integration
nxr shell backend
```

Conceptually:

```bash
nix develop .#backend -c ...
```

This is for operations that intentionally require the shell environment. It should not be the default for well-authored apps.

Policies:

- `inherit` — use caller environment;
- `clean` — use reduced environment;
- `devShell` — execute in a selected development shell;
- `explicit` — pass only configured variables.

### 2.6 Automatic shell integration from dev shells

The optional Nix module can:

- add `nxr` to the shell;
- add shell-specific integration scripts;
- register dynamic app/task completion;
- configure cache paths;
- expose a prompt indicator if desired.

With direnv:

```bash
use flake
```

loading the development shell also activates `nxr` completion for that shell session.

No global shell configuration is modified.

### 2.7 Rich output handling

Modes:

```bash
nxr --output live task dev
nxr --output grouped task ci
nxr --output failures task ci
nxr --output summary task ci
nxr --events jsonl task ci
```

Features:

- node prefixes;
- timestamps;
- duration reporting;
- progress state;
- terminal-width adaptation;
- ANSI-aware truncation;
- saved logs;
- failure excerpts;
- raw child passthrough;
- non-TTY CI mode;
- machine-readable event streams.

### 2.8 Process supervision

Required capabilities:

- Unix process groups;
- graceful shutdown deadlines;
- signal escalation;
- Windows job-object abstraction;
- background process cleanup;
- interactive-child focus;
- terminal resize propagation;
- restart generation tracking;
- orphan prevention.

### 2.9 Argument schemas

Optional metadata may document app arguments for completion and help:

```nix
apps.deploy-staging.arguments = {
  region = {
    type = "string";
    completion = [ "us-west-2" "us-east-1" ];
  };
};
```

This is descriptive metadata. `nxr` should avoid becoming a second application parser. The underlying app remains authoritative.

### 2.10 Categories and aliases

Optional metadata:

```nix
categories = {
  quality = [ "fmt-check" "lint" "test" ];
  deployment = [ "deploy-staging" "deploy-production" ];
};

aliases = {
  t = "test";
  ci = "task:ci";
};
```

Aliases must be explicit and inspectable.

### 2.11 Task status summaries

```text
TASK          STATUS      DURATION
fmt-check     succeeded   1.2s
lint          succeeded   4.8s
test          failed      19.4s
package       skipped     -
```

JSON output includes exact exit codes and timestamps.

### 2.12 Configuration inspection

```bash
nxr inspect
nxr inspect app test
nxr inspect task ci
nxr inspect shell default
```

The command reports normalized configuration after defaults and capability resolution.

## 3. Possible post-V2 extensions

These are explicitly not committed roadmap items. Preserved V3 design prose lives in [ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md).

- readiness probes for long-running services;
- task matrices;
- remote execution adapters;
- daemon-backed warm evaluation;
- editor protocol;
- graphical process dashboard;
- Nix check/app linkage metadata;
- reproducible ephemeral service environments;
- CI workflow generation;
- policy plugins;
- first-class projects and affected analysis;
- action contracts, artifact caches, and result replay;
- provider-independent CI plans and test intelligence;
- optional local daemon and remote worker fabric.

Each should require evidence that it belongs in `nxr` rather than another layer.
