# Intended Tech Stack and Repository Shape

## 1. Implementation language

Use Rust.

Reasons:

- fast startup;
- one static or mostly self-contained binary;
- strong process and error handling;
- good cross-platform abstractions;
- mature CLI ecosystem;
- straightforward JSON and event modeling;
- safe argument-vector construction;
- suitable foundation for a future TUI without requiring one in V1.

## 2. Core crates

Recommended baseline:

```text
clap                 CLI parsing and generated static completion
clap_complete        Bash/Zsh/Fish completion generation
serde                data model serialization
serde_json           Nix JSON and machine output
thiserror            library error types
miette               rich diagnostics and source-aware reports
tracing              internal structured instrumentation
tracing-subscriber   human and JSON runner logs
tokio                V2 async scheduler and process supervision
which                executable discovery
camino               UTF-8 paths where appropriate
directories          cache/config paths
fs2 or file-locking  discovery cache locking
notify               V2 filesystem watching
petgraph             V2 DAG validation and traversal
console              terminal capability helpers
indicatif             optional progress rendering, used carefully
anstyle              color and styling abstraction
shell-words          only for display/parsing controlled text, never execution
nix                  Unix signals/process primitives if needed
windows               Windows job objects in later platform work
insta                snapshot tests
assert_cmd            CLI integration tests
predicates            test assertions
tempfile              fixture workspaces
```

Crate selection should remain conservative. Avoid adding a full TUI stack until the selector or process dashboard clearly needs it.

## 3. Nix integration strategy

V1 shells out to the `nix` executable.

Reasons:

- stable command boundary compared with internal C++ APIs;
- compatibility with user Nix configuration;
- inherits trust settings, stores, substituters, and daemon behavior;
- avoids linking against unstable implementation internals;
- easier packaging.

The adapter should use structured output:

```text
nix flake show --json
nix eval --json
nix run
nix develop
nix print-dev-env
```

The exact command set may vary by supported Nix version and should be hidden behind capability-tested adapter methods.

## 4. Workspace layout

```text
nxr/
├── Cargo.toml
├── Cargo.lock
├── flake.nix
├── flake.lock
├── README.md
├── LICENSE
├── CHANGELOG.md
├── CONTRIBUTING.md
├── deny.toml
├── rust-toolchain.toml
├── .envrc
├── .github/
│   └── workflows/
│       ├── ci.yml
│       ├── release.yml
│       └── docs.yml
├── crates/
│   ├── nxr-cli/
│   │   └── src/
│   │       ├── main.rs
│   │       ├── commands/
│   │       └── output.rs
│   ├── nxr-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── model.rs
│   │       ├── diagnostics.rs
│   │       └── config.rs
│   ├── nxr-nix/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── adapter.rs
│   │       ├── capabilities.rs
│   │       ├── discovery.rs
│   │       └── command.rs
│   ├── nxr-process/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── foreground.rs
│   │       ├── supervisor.rs
│   │       └── signals.rs
│   ├── nxr-workspace/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── discovery.rs
│   │       └── paths.rs
│   ├── nxr-completion/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── cache.rs
│   │       └── dynamic.rs
│   ├── nxr-task/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── schema.rs
│   │       ├── graph.rs
│   │       ├── planner.rs
│   │       └── scheduler.rs
│   └── nxr-watch/
│       └── src/
│           ├── lib.rs
│           ├── watcher.rs
│           └── restart.rs
├── nix/
│   ├── lib/
│   │   ├── default.nix
│   │   ├── mk-app.nix
│   │   └── metadata.nix
│   ├── modules/
│   │   ├── flake-parts.nix
│   │   ├── apps.nix
│   │   ├── tasks.nix
│   │   └── shell-integration.nix
│   └── packages/
│       └── nxr.nix
├── shell/
│   ├── nxr.bash
│   ├── nxr.zsh
│   └── nxr.fish
├── docs/
│   ├── architecture.md
│   ├── cli.md
│   ├── app-authoring.md
│   ├── task-schema.md
│   ├── shell-integration.md
│   └── adr/
│       ├── README.md
│       ├── template.md
│       └── NNNN-decision-title.md
├── schemas/
│   ├── list-v1.schema.json
│   ├── plan-v1.schema.json
│   ├── execution-plan-v1.schema.json
│   ├── events-v1.schema.json
│   └── task-v1.schema.json
├── fixtures/
│   ├── basic-apps/
│   ├── app-metadata/
│   ├── nested-directory/
│   ├── broken-flake/
│   ├── named-dev-shells/
│   ├── task-dag/
│   ├── parallel-group/
│   └── watch-project/
├── tests/
│   ├── cli/
│   ├── nix-integration/
│   ├── process/
│   └── compatibility/
└── xtask/
    └── src/
        └── main.rs
```

## 5. Crate boundaries

### nxr-cli

Owns:

- CLI grammar;
- command dispatch;
- user-facing renderers;
- top-level exit codes.

Does not own Nix command construction or graph algorithms.

### nxr-core

Owns:

- shared models;
- schema versions;
- diagnostics;
- environment and working-directory policy types.

### nxr-nix

Owns:

- Nix executable discovery;
- version/capability detection;
- flake app evaluation;
- app resolution;
- Nix argument-vector construction;
- parsing structured Nix results.

### nxr-process

Owns:

- foreground execution;
- signal forwarding;
- child process groups;
- exit status;
- V2 supervision.

### nxr-workspace

Owns:

- upward flake discovery;
- path normalization;
- invocation/root relationships;
- repository context.

### nxr-completion

Owns:

- shell script generation;
- dynamic candidate protocol;
- discovery cache;
- timeout behavior.

### nxr-task

V2 crate owning:

- task schema;
- graph validation;
- planning;
- scheduler;
- task result calculation.

### nxr-watch

V2 crate owning:

- filesystem events;
- debounce;
- restart orchestration;
- watch generation state.

## 6. Async strategy

V1 foreground execution does not need an async runtime everywhere.

However, V2 parallelism and watching do.

Recommended approach:

- keep data modeling and command construction synchronous;
- use Tokio in the CLI orchestration layer;
- isolate blocking Nix evaluation with `spawn_blocking` or dedicated processes;
- use bounded channels for event flow;
- avoid forcing every small library API to become async.

## 7. Output architecture

Internal event model lives in `nxr-core` or a dedicated crate.

Renderers:

```text
HumanRenderer
PlainRenderer
JsonRenderer
JsonLinesEventRenderer
GroupedTaskRenderer
FailureOnlyRenderer
```

Terminal rendering should be testable with snapshots.

Child raw output should bypass UTF-8 assumptions when using raw mode.

## 8. Nix library shape

Public flake outputs:

```nix
{
  packages.<system>.nxr = ...;
  apps.<system>.default = ...;
  lib.mkApp = ...;
  flakeModules.default = ...;
}
```

The flake-parts module may expose options:

```text
perSystem.nxr.apps
perSystem.nxr.tasks
perSystem.nxr.shellIntegration
perSystem.nxr.settings
```

It emits ordinary apps plus the versioned `nxr.<system>` metadata output.

## 9. Testing strategy

### Unit tests

- app-name parsing;
- flake-reference parsing;
- command argument construction;
- working-directory resolution;
- task graph cycle detection;
- environment policy merging;
- exit-code calculation.

### Snapshot tests

- human app list;
- diagnostics;
- plans;
- graph rendering;
- task summaries;
- completion output.

### Integration tests

Run against fixture flakes:

- app execution;
- nested directory invocation;
- argument preservation;
- environment inheritance;
- clean environment;
- signal interruption;
- remote-like path references;
- named development shell;
- parallel task group;
- watcher restart.

### Compatibility matrix

CI should test selected supported Nix versions and platforms.

Initial flake targets (all four evaluate; CI exercises a subset):

- aarch64-darwin;
- x86_64-darwin;
- aarch64-linux;
- x86_64-linux.

Later:

- Windows when a realistic Nix execution story is defined.

## 10. Release engineering

Use:

- semantic versioning;
- generated changelog;
- signed release artifacts where practical;
- GitHub Releases;
- flake package;
- crates.io publication only if library crates are intentionally public;
- SBOM and checksums;
- reproducible Nix builds;
- shell completion assets included in packages.

## 11. Performance targets

V1 warm-path goals:

- root discovery: effectively instantaneous;
- cached app listing: under 50 ms;
- uncached listing: dominated by one Nix evaluation;
- execution overhead after Nix resolution: negligible;
- completion timeout: short enough not to block interactive use.

V2:

- event rendering must not become the bottleneck;
- watcher debounce should avoid duplicate generations;
- parallel scheduler overhead should be trivial relative to child operations.

## 12. Dependency policy

- prefer maintained crates with small dependency surfaces;
- deny known vulnerabilities and duplicate major versions where practical;
- avoid shelling out to optional Unix utilities;
- implement platform behavior through Rust APIs;
- keep the core runner usable without fuzzy-picker or TUI dependencies;
- feature-gate optional presentation layers.
