# Architecture

## 1. System purpose

`nxr` is an execution and presentation layer over Nix flake apps.

Its V1 responsibilities are:

1. locate a flake;
2. evaluate its app outputs;
3. present those apps to humans and tools;
4. resolve one selected app;
5. invoke it through Nix;
6. supervise the resulting process;
7. preserve exit status, signals, arguments, and terminal behavior.

Its V2 responsibilities extend to orchestration:

1. resolve an optional task graph;
2. plan dependency execution;
3. execute serial and parallel nodes;
4. supervise multiple child processes;
5. multiplex structured output;
6. run and restart watched tasks;
7. optionally execute nodes within named development shells.

`nxr` does not replace Nix evaluation or realization. The Nix CLI remains the execution backend in V1.

## 2. Layer model

```text
┌─────────────────────────────────────────────────────────────┐
│ User interfaces                                             │
│ CLI, completion, fuzzy picker, JSON output, event stream    │
├─────────────────────────────────────────────────────────────┤
│ nxr orchestration                                           │
│ discovery, resolution, policy, process supervision, DAG     │
├─────────────────────────────────────────────────────────────┤
│ Nix command boundary                                        │
│ flake show/eval, run, develop, build, print-dev-env          │
├─────────────────────────────────────────────────────────────┤
│ Project flake                                               │
│ apps, packages, checks, devShells, optional nxr metadata     │
├─────────────────────────────────────────────────────────────┤
│ Nix store and daemon                                        │
│ evaluation, realization, substitution, build sandboxing      │
├─────────────────────────────────────────────────────────────┤
│ Execution host                                              │
│ local machine, CI runner, DevPod, VM, devcontainer           │
└─────────────────────────────────────────────────────────────┘
```

## 3. Canonical project objects

### 3.1 Flake apps

Leaf executable operations are standard flake apps:

```nix
apps.${system}.test = {
  type = "app";
  program = "${testPackage}/bin/project-test";
  meta.description = "Run the test suite";
};
```

The app is the canonical operation. `nxr test` is shorthand for resolving and invoking that app.

### 3.2 Development shells

Development shells define interactive workspaces:

```nix
devShells.${system}.default = pkgs.mkShell {
  packages = [
    pkgs.cargo
    pkgs.rust-analyzer
    pkgs.nxr
  ];

  DATABASE_URL = "postgres://localhost/project";
};
```

Apps and development shells are siblings, not parent and child objects. A normal app should not require the caller to enter a development shell merely to find its executables.

### 3.3 Checks

Checks define sandboxed, cacheable validation:

```nix
checks.${system}.test = pkgs.runCommand "test-check" {
  nativeBuildInputs = [ pkgs.cargo pkgs.cargo-nextest ];
} ''
  cp -R ${./.} source
  chmod -R +w source
  cd source
  cargo nextest run
  touch $out
'';
```

A project may expose both:

```text
apps.test      fast mutable developer operation
checks.test    hermetic validation artifact
```

`nxr` should not pretend that running an app turns it into a cached Nix build.

### 3.4 Optional nxr metadata

V1 reads standard app metadata where available:

```nix
meta.description = "Run the test suite";
```

V2 may define an additive output:

```nix
nxr.${system} = {
  schemaVersion = 1;

  tasks.ci = {
    description = "Run the complete CI workflow";
    dependsOn = [ "fmt-check" "lint" "test" ];
  };
};
```

The exact schema must be versioned and documented. Standard apps remain executable without it.

## 4. V1 component architecture

```text
main
 ├── cli
 │    ├── argument parser
 │    ├── output mode selection
 │    └── command dispatch
 ├── workspace
 │    ├── flake-root discovery
 │    ├── repository context
 │    └── original working directory
 ├── nix
 │    ├── capability detection
 │    ├── app discovery
 │    ├── app resolution
 │    └── command execution
 ├── model
 │    ├── FlakeRef
 │    ├── App
 │    ├── ResolvedApp
 │    └── Diagnostic
 ├── process
 │    ├── terminal mode
 │    ├── signal forwarding
 │    ├── exit-code propagation
 │    └── child cleanup
 ├── presentation
 │    ├── human renderer
 │    ├── JSON renderer
 │    └── event renderer
 └── completion
      ├── bash
      ├── zsh
      ├── fish
      └── generated dynamic candidates
```

### 4.1 CLI parser

The parser must preserve a strict boundary between `nxr` flags and app arguments.

```bash
nxr [global options] <app> [--] [app arguments...]
```

After the app name, unknown arguments should be treated as application arguments unless an explicitly documented `nxr` subcommand is active. `--` always terminates `nxr` parsing.

### 4.2 Workspace discovery

When invoked from a nested directory, `nxr` searches upward for `flake.nix`.

It records:

- invocation directory;
- discovered flake root;
- Git repository root, if different;
- selected flake reference;
- whether the target is local or remote.

Default execution should preserve the caller's working directory. An app can therefore operate naturally on the current subdirectory.

A task may explicitly request root execution in V2.

#### WorkspaceSnapshot

Task and multi-app orchestration evaluate the workspace **once** into a
`WorkspaceSnapshot`: flake selection, Nix adapter (locate + `currentSystem`),
discovered apps, and optional task document. Every task node is prepared into a
`PreparedTaskNode` **before** the scheduler starts. Scheduler execution must not
re-run `flake show`, task eval, or system detection.

Bare `nxr <app>` / `nxr run <app>` use a **fast path**: construct
`nix run <flake>#<app>` without `flake show`. On a nonzero Nix exit, `nxr` may
optionally discover apps to emit "did you mean?" suggestions when the name is
absent. Commands that must know the app catalog (`list`, `plan`, `doctor`,
completion) still discover explicitly.

### 4.3 Nix adapter

V1 shells out to the installed Nix CLI rather than linking unstable internal Nix libraries.

The adapter is responsible for:

- checking that `nix` exists;
- detecting supported flags and output formats;
- discovering apps using stable JSON output where possible;
- constructing exact `nix run` argument vectors;
- avoiding shell interpolation;
- preserving `--` and subsequent arguments;
- collecting structured diagnostics when available.

The adapter must never build a command string such as:

```text
"nix run " + user_input
```

It constructs an argument vector and spawns the process directly.

### 4.4 App discovery

Discovery should prefer one structured evaluation request over repeatedly invoking Nix per app.

The discovery result is normalized into:

```rust
struct App {
    name: String,
    attr_path: String,
    flake_ref: String,
    system: String,
    description: Option<String>,
    is_default: bool,
    metadata: BTreeMap<String, JsonValue>,
}
```

Discovery must handle:

- no apps;
- an unavailable current-system app set;
- evaluation errors;
- unknown metadata fields;
- hidden or internal apps in future schema versions;
- default apps;
- remote flakes.

### 4.5 Process execution

Interactive apps should inherit the terminal directly by default.

Required behavior:

- stdin, stdout, and stderr inherited in plain mode;
- child exit status returned by `nxr`;
- `SIGINT`, `SIGTERM`, and terminal resize behavior preserved;
- no extra buffering for interactive commands;
- child process group used where supported;
- orphaned descendants cleaned up when `nxr` owns supervision;
- Windows support designed behind a platform abstraction, even if initial releases prioritize Unix.

For simple V1 execution, replacing the runner process with the child on Unix is attractive because signal behavior becomes native. However, `exec` prevents post-run summaries and richer event capture. The implementation should support two strategies:

1. **transparent exec mode** for maximum fidelity;
2. **supervised mode** for structured output and V2 features.

## 5. Environment model

### 5.1 Default inheritance

By default, an app inherits the caller's environment through `nix run`.

This allows `.envrc` and an active development shell to provide:

- credentials;
- service endpoints;
- project configuration;
- editor and terminal state;
- language cache locations;
- local socket paths.

### 5.2 Executable dependencies

Apps should include executable dependencies in their own closure through helpers such as `writeShellApplication`.

The expected model is:

```text
app closure       exact executable dependencies
caller environment runtime state, secrets, local endpoints
dev shell         interactive convenience and editor tooling
```

### 5.3 Clean execution

`nxr run --clean-env` or `nxr --clean-env <app>` should provide an explicit reduced-environment mode.

This is useful for:

- detecting accidental dependence on development-shell `PATH`;
- reproducing CI failures;
- security-sensitive operations;
- app authoring diagnostics.

The exact allowlist must be documented and inspectable.

## 6. V2 task architecture

### 6.1 Tasks and apps

A V2 task is orchestration metadata. A task node may invoke:

- an app in the current flake;
- an app in another flake;
- another task;
- an explicit command, if enabled;
- a named development-shell command;
- a grouped set of nodes.

Standard apps should remain the preferred leaf nodes.

Example conceptual schema:

```nix
nxr.${system}.tasks = {
  ci = {
    description = "Run CI validation";
    dependsOn = [ "fmt-check" "lint" "test" ];
    mode = "parallel";
  };

  dev = {
    description = "Start local services";
    group = {
      mode = "parallel";
      failFast = true;
      members = [ "api" "web" "worker" ];
    };
  };

  api = {
    app = "api-dev";
    watch = {
      paths = [ "crates/api" ];
      restart = "on-change";
    };
  };
};
```

### 6.2 Planner

The planner converts task metadata into an immutable execution plan.

It must:

- validate task references;
- reject cycles with a useful cycle path;
- resolve app references before execution;
- calculate dependency ordering;
- identify serial and parallel regions;
- apply concurrency limits;
- record environment and working-directory policy;
- produce a machine-readable plan.

### 6.3 Scheduler

The scheduler operates on node states:

```text
pending
ready
running
succeeded
failed
cancelled
skipped
restarting
```

Required policies:

- fail-fast or continue-on-error;
- global concurrency limit;
- group-level concurrency limit;
- dependent cancellation;
- stable output ordering in summarized mode;
- immediate streaming in live mode;
- deterministic final result calculation.

### 6.4 Process supervisor

V2 requires a dedicated supervisor supporting:

- multiple process groups;
- terminal and non-terminal children;
- graceful shutdown deadlines;
- escalation from interrupt to termination to kill;
- restart-on-change;
- restart-on-failure;
- child log routing;
- process labels;
- background service lifecycle;
- dependency readiness gates in later V2.x releases.

### 6.5 Watch service

Watch mode is orchestration, not Nix evaluation polling.

The watcher should:

- use native filesystem notifications;
- debounce changes;
- support include and exclude globs;
- restart one node or invalidate a dependent subgraph;
- avoid watching Nix store paths;
- detect atomic-save patterns;
- avoid duplicate restarts during large repository operations;
- terminate old process trees before restarting;
- optionally coalesce output between generations.

### 6.6 Output event bus

V2 execution emits internal events:

```rust
enum Event {
    PlanCreated,
    NodeQueued,
    NodeStarted,
    StdoutChunk,
    StderrChunk,
    NodeRestarting,
    NodeExited,
    NodeSkipped,
    RunCompleted,
    Diagnostic,
}
```

Renderers consume the same events:

- interactive terminal renderer;
- plain CI renderer;
- JSON-lines renderer;
- compact summary renderer;
- future TUI renderer.

This avoids coupling scheduler logic to terminal presentation.

## 7. Caching boundaries

`nxr` must be explicit about what is cached.

### Nix handles

- flake input locking;
- evaluation caching;
- store realization;
- binary substitution;
- derivation build caching;
- checks and package artifacts.

### Native tools handle

- Cargo incremental compilation;
- TypeScript build caches;
- test runner caches;
- bundler caches;
- framework development caches.

### nxr may cache

- app discovery metadata;
- shell completion candidates;
- normalized flake metadata;
- Nix capability detection.

V2 should not introduce a second opaque artifact cache unless a concrete use case justifies it. A task graph is not automatically a build cache.

## 8. Security model

The user is executing code from a flake. `nxr` must not imply otherwise.

Security requirements:

- display remote flake references clearly;
- never interpolate app names into a shell;
- do not silently load arbitrary project configuration outside the flake;
- version custom metadata schemas;
- expose the exact command with `--dry-run` or `plan`;
- allow restricted environment execution;
- preserve Nix trust and substituter behavior rather than bypassing it;
- avoid evaluating unrelated outputs when possible;
- do not automatically execute shell hooks merely for app discovery;
- treat descriptions and metadata as untrusted terminal text and sanitize control sequences.

## 9. Compatibility strategy

The project should define a minimum supported Nix version and test across a rolling compatibility matrix.

Compatibility should be implemented through:

- runtime capability detection;
- a narrow Nix adapter;
- golden tests for command construction;
- integration fixtures covering multiple flake shapes;
- graceful fallback when optional metadata is unavailable.

Direct linkage to internal Nix libraries should not be required for V1.

V2.x extension points (metadata adapters, capability-negotiated Nix, versioned
event/schema surfaces) are documented in [COMPATIBILITY.md](COMPATIBILITY.md).
Adapters must not create a second authoritative operation definition; flake
apps remain canonical leaf operations.

## 10. Ecosystem positioning

The architecture is intentionally layered across established Nix workflows:

```text
direnv/nix-direnv
  activates a development shell

development shell
  supplies interactive tools, environment, and nxr integration

flake apps
  provide canonical executable leaf operations

nxr task graph
  coordinates apps

nxr project/action graph
  enables affected analysis, CI, caching, and remote execution

Nix store/builders
  remain authoritative for derivations and immutable Nix artifacts
```

Features borrowed from adjacent task runners must be adapted to this layer model. They must not introduce a second toolchain resolver or make standard flake outputs subordinate to an opaque runner database.
