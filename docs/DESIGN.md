# Design

## 1. Product thesis

The Nix ecosystem already has a strong executable abstraction: the flake app.

The missing layer is not another task language. It is a high-quality command experience around existing apps.

`nxr` therefore begins with a strong constraint:

> A valid flake app is sufficient configuration.

Everything else is optional metadata or orchestration.


### 1.1 Ecosystem synthesis target

The design should intentionally combine the strongest ideas of adjacent tools:

```text
just and mission-control  command discovery and low-friction invocation
mise and Taskfile         task metadata, DAGs, watch, validation, output modes
devenv                    task/process lifecycle and readiness
direnv and nix-direnv     automatic session-local shell activation
Nx, moonrepo, Turborepo   project graphs, affected execution, caching, CI scaling
Bazel and Pants           action contracts, graph queries, remote execution, events
Nix                       locked inputs, executable closures, checks, stores, builders
```

The synthesis is constrained by one rule:

> Add workflow semantics around Nix primitives; do not replace those primitives with a parallel package, runtime, or environment model.

This means `nxr` should be immediately useful in an ordinary flake repository, progressively adoptable, and removable without making the repository inoperable.

## 2. Why use one app per operation

A project can expose:

```text
.#build
.#test
.#lint
.#fmt
.#dev
.#db-migrate
.#db-reset
.#generate
.#deploy-staging
```

This has several advantages.

### 2.1 Remote addressability

The operation can be invoked from a local directory or a remote reference:

```bash
nix run .#test
nix run github:owner/project#test
```

### 2.2 Exact runtime closure

The operation can carry its required binaries independently of an interactive shell.

### 2.3 Stable public interface

The flake documents what the project can do in a machine-readable form.

### 2.4 Cross-environment consistency

The same operation can be used:

- by a developer;
- by CI;
- in a DevPod;
- in a devcontainer;
- on a remote development VM;
- by another flake or automation system.

### 2.5 Escape hatch remains native

Removing `nxr` does not remove the operation:

```bash
nxr test
# remains equivalent to
nix run .#test
```

## 3. App versus task

An app is an executable entry point.

A task is an orchestration node.

V1 needs only apps. V2 introduces tasks for cases that cannot be modeled ergonomically as one app invocation:

- dependency graphs;
- parallel service groups;
- watchers and restart policies;
- shared output presentation;
- lifecycle supervision;
- explicit shell/environment policies.

The distinction should remain visible:

```text
app   = executable capability
task  = plan for coordinating capabilities
```

A task should generally call apps rather than duplicate their implementation.

## 4. Development shell relationship

### 4.1 Independent outputs

An app is not automatically run inside a development shell.

```text
apps.<system>.test
devShells.<system>.default
```

are separate outputs.

### 4.2 Recommended dependency ownership

Use this rule:

> The development shell is for humans. The app closure is for the app.

The app should include its executable runtime dependencies.

The development shell should include:

- editor integrations;
- language servers;
- interactive debuggers;
- project-wide tools;
- `nxr`;
- shell completion integration;
- optional local service helpers;
- environment initialization.

### 4.3 Environment inheritance

Apps may intentionally consume runtime state from the caller:

- secrets;
- local URLs;
- sockets;
- temporary paths;
- CI variables;
- cloud credentials.

This is different from depending accidentally on a binary that only exists on the caller's `PATH`.

## 5. No mandatory second manifest

V1 must not require:

```text
nxr.toml
nxr.yaml
.nxr.json
```

because this would immediately create synchronization problems:

```text
flake apps say one thing
runner manifest says another
```

Optional non-Nix configuration may be considered later only for user-global preferences, never as the canonical project operation list.

## 6. Metadata strategy

### 6.1 Standard metadata first

Use standard app fields where available:

```nix
{
  type = "app";
  program = "...";
  meta.description = "Run the test suite";
}
```

### 6.2 Names should remain CLI-friendly

Recommended app names:

```text
test
lint
fmt
fmt-check
dev
db-migrate
deploy-staging
```

Avoid encoding a hierarchy that requires complex parsing. V2 may display logical categories separately.

### 6.3 Additive custom metadata

V2 can use a versioned custom output:

```nix
nxr.${system} = {
  schemaVersion = 1;
  tasks = { ... };
  apps = {
    deploy-staging = {
      category = "deployment";
      dangerous = true;
      confirmation = "typed";
    };
  };
};
```

The runner must ignore unknown fields and reject unsupported major schema versions with a precise message.

## 7. Working-directory semantics

This is a surprisingly important contract.

### Default

`nxr` discovers the flake root but preserves the user's current working directory.

Example:

```text
repo/
  flake.nix
  crates/api/
```

From `repo/crates/api`:

```bash
nxr test
```

the app starts in `repo/crates/api`, not automatically in `repo`.

This matches normal shell expectations and allows subtree-aware tools.

### Explicit root mode

V1 provides:

```bash
nxr --root test
```

V2 task metadata may declare:

```nix
workingDirectory = "flake-root";
```

or:

```nix
workingDirectory = "invocation";
```

Arbitrary relative directories should be resolved against the flake root.

## 8. Argument-forwarding semantics

Argument forwarding must be boring and exact.

These should all work:

```bash
nxr test
nxr test --nocapture
nxr test -- --nocapture
nxr deploy --region us-west-2
nxr command -- --flag=value "two words"
```

The implementation must spawn argument vectors without shell reparsing.

The documented parsing rule is:

1. parse global `nxr` flags before the app name;
2. select the app;
3. treat remaining arguments as app arguments;
4. remove one explicit `--` separator if present;
5. never interpret app arguments as shell syntax.

Subcommands such as `nxr list` and `nxr doctor` occupy reserved command positions. An app with a conflicting name remains invocable through:

```bash
nxr run list
```

## 9. Output philosophy

### 9.1 Do not damage interactive tools

By default, app processes inherit the terminal.

`nxr` must not wrap every program in line buffering or strip ANSI behavior.

### 9.2 Human-readable runner messages

Runner messages should be brief:

```text
✓ .#test
```

Detailed diagnostics appear only when needed or requested.

### 9.3 Stable machine output

Commands that expose data support:

```bash
nxr list --json
nxr plan --json
nxr run --events=jsonl test
```

JSON output must be versioned.

### 9.4 V2 multiplexed output

Parallel tasks need labeled output:

```text
[api] listening on :8080
[web] ready in 421 ms
[worker] connected to queue
```

The renderer should support:

- live interleaving;
- grouped output by task;
- quiet-success mode;
- failure-only mode;
- timestamps;
- raw mode;
- JSON lines.

## 10. Process lifecycle

### Foreground app

The app behaves like a direct child command:

- Ctrl-C reaches it;
- terminal resizing works;
- exit status is preserved;
- stdin remains usable.

### Parallel group

The runner owns the group:

- one Ctrl-C initiates graceful shutdown;
- all children receive termination;
- a second Ctrl-C escalates;
- no background child is intentionally left running;
- final exit status follows documented group policy;
- caller stdin is closed for every supervised child (no shared ownership).

Serial interactive task runs (`-j 1` without `--output` / `--events`) still inherit stdin.

### Watch task

The runner owns generations:

- old generation terminates before replacement;
- change events are debounced;
- rapid saves coalesce;
- logs identify restart boundaries;
- shutdown cleans both watcher and child tree.

## 11. V2 DAG semantics

### 11.1 Dependency meaning

If task `deploy` depends on `test`, `test` must complete successfully before `deploy` starts.

### 11.2 Parallelism

Sibling dependencies may run in parallel when no dependency edge orders them.

### 11.3 Failure

Default behavior:

- failed node marks dependents skipped;
- independent branches may continue unless fail-fast is enabled;
- final run fails if any required node failed.

### 11.4 Cycles

Cycles are configuration errors.

The error should show an actual path:

```text
cycle detected:
  test -> generate -> schema -> test
```

### 11.5 Repeated dependencies

A node executes once per plan unless explicitly declared reentrant.

### 11.6 Parameterized tasks

Parameterized graph nodes are deferred beyond initial V2 unless a simple, deterministic schema can be designed. Arbitrary dynamic graph generation would make plans hard to inspect and cache.

## 12. Shell integration

`nxr` should provide generated integration for Bash, Zsh, and Fish.

The shell integration supports:

- completion of app and task names;
- descriptions in completion menus;
- optional abbreviations;
- cached dynamic discovery;
- invalidation when `flake.nix` or `flake.lock` changes;
- discovery from nested directories.

The development shell can install and activate this integration automatically.

For example, an `nxr` flake-parts module may provide:

```nix
nxr.shellIntegration.enable = true;
```

and augment the dev shell with a shell hook.

Direnv loading the dev shell then makes completion available automatically.

This should be opt-in and shell-safe. It must not mutate user-global shell files.

## 13. Dangerous operations

Deployment and destructive database apps are still ordinary apps, but optional metadata may improve UX:

```nix
nxr.${system}.apps.db-reset = {
  dangerous = true;
  confirmation = "typed";
  confirmationText = "reset";
};
```

Rules:

- non-interactive execution must require an explicit override;
- `--yes` behavior must be documented;
- `nxr` never claims to make the underlying operation safe;
- native `nix run .#db-reset` remains possible and bypasses runner-specific confirmation.

Therefore dangerous-operation metadata is a convenience guardrail, not a security boundary.

## 14. Decisions intentionally deferred

The following are not V1 concerns:

- a custom remote execution protocol;
- task result caching;
- distributed scheduling;
- a daemon;
- graphical UI;
- secrets storage;
- deployment state management;
- workflow marketplace;
- container lifecycle management.

These can be revisited only if concrete demand appears.
