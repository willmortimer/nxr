# Roadmap

## Roadmap principles

1. Ship a small, trustworthy runner before inventing a task language.
2. Preserve direct `nix run` compatibility at every stage.
3. Lock CLI semantics early: argument forwarding, exit codes, working directory, output streams, and signal behavior.
4. Build structured internals before rich presentation.
5. Add orchestration only after foreground app execution is excellent.
6. Treat adjacent tools as design inputs, but express the resulting workflow through Nix-native primitives.
7. Preserve a gradual path from a standard app to a remotely executable action.
8. Keep local development and CI on the same inspectable execution graph.

# V1 — Standard Flake App Runner

## Phase 0: repository foundation

### Deliverables

- Rust workspace;
- Nix flake for development and packaging;
- CI on macOS ARM and Linux x86_64;
- formatting, linting, tests, dependency checks;
- architecture decision records;
- release process skeleton;
- fixture flake framework.

### Exit criteria

- `nix develop` enters a complete contributor shell;
- `nix build` produces `nxr`;
- CI verifies the same operations through flake apps;
- project can cut a versioned prerelease.

## Phase 1: flake discovery and app listing

### Deliverables

- upward `flake.nix` discovery;
- explicit `--flake`;
- Nix executable detection;
- current-system detection;
- structured app discovery;
- normalized app model;
- human `nxr list`;
- JSON `nxr list --json`;
- app descriptions;
- default app detection;
- clear evaluation diagnostics.

### Commands

```bash
nxr
nxr list
nxr list --json
nxr --flake ../project list
```

### Exit criteria

- works from repository root and nested directories;
- performs at most one normal discovery evaluation per invocation;
- lists apps deterministically;
- handles flakes with no apps;
- JSON schema is documented and tested.

## Phase 2: foreground app execution

### Deliverables

- `nxr <app>`;
- `nxr run <app>`;
- exact argument forwarding;
- `--` behavior;
- current-directory preservation;
- `--root` and `--cwd`;
- exit-code propagation;
- signal forwarding;
- TTY inheritance;
- remote flake references;
- plan/dry-run support.

### Commands

```bash
nxr test
nxr test -- --nocapture
nxr --root test
nxr --flake github:owner/project test
nxr plan test
```

### Exit criteria

- interactive tools behave like direct execution;
- Ctrl-C behavior is correct;
- arbitrary arguments are not shell-evaluated;
- child exit codes are preserved;
- no orphan child remains after interruption;
- exact Nix command is visible through `plan`.

## Phase 3: ergonomic discovery

### Deliverables

- shell completion for Bash, Zsh, and Fish;
- dynamic current-flake candidates;
- descriptions in completion;
- fuzzy selector;
- app-not-found suggestions;
- discovery cache;
- `--refresh`;
- completion timeout and fallback.

### Commands

```bash
nxr completion zsh
nxr --select
nxr refresh
```

### Exit criteria

- completion is responsive in a warm project;
- no diagnostic noise corrupts shell completion;
- cache invalidates after `flake.nix` or `flake.lock` changes;
- selector is optional and does not burden headless use.

## Phase 4: output and diagnostics

### Deliverables

- human, plain, and JSON runner logs;
- quiet and verbose modes;
- no-color behavior;
- stable exit codes;
- rich Nix error summaries;
- sanitized project-provided text;
- non-TTY behavior;
- machine-readable plan schema.

### Exit criteria

- stdout remains usable in pipelines;
- runner messages do not corrupt app stdout;
- CI logs remain readable without terminal control;
- all errors include actionable context.

## Phase 5: doctor and app-authoring library

### Deliverables

- `nxr doctor`;
- static app validation;
- clean-environment validation;
- Nix `mkApp` helper;
- flake-parts app module;
- app naming and description guidance;
- example repositories;
- migration guide from `mise`, `just`, and shell aliases.

### Commands

```bash
nxr doctor
nxr doctor test
nxr doctor --clean-env test
```

### Exit criteria

- normal doctor mode does not execute destructive apps;
- accidental development-shell PATH dependencies can be detected;
- helper emits ordinary standard apps;
- no helper is required for runner compatibility.

## Phase 6: V1 stabilization

### Deliverables

- compatibility matrix;
- performance profiling;
- release packaging;
- man page;
- complete CLI reference;
- telemetry decision documented, defaulting to none;
- security review;
- V1.0 release.

### V1.0 acceptance criteria

A user can clone a flake-based repository and run:

```bash
nxr
nxr test
nxr test -- --nocapture
```

without another task manifest, while retaining equivalent native `nix run` commands.

# V2 — Workflow Orchestration

## Phase 7: versioned metadata foundation

### Deliverables

- `nxr.<system>.schemaVersion`;
- normalized task schema;
- custom app metadata;
- schema validation;
- `nxr inspect`;
- JSON schemas;
- unsupported-version diagnostics;
- flake-parts task options.

### Task schema initial fields

```text
description
dependsOn
app
workingDirectory
environment
failurePolicy
concurrency
hidden
category
arguments
```

### Exit criteria

- metadata is optional;
- unknown fields are tolerated;
- unsupported major versions fail clearly;
- leaf apps remain directly runnable.

## Phase 8: DAG planner

### Deliverables

- task graph parsing;
- dependency resolution;
- cycle detection;
- serial plan;
- parallelizable region calculation;
- plan JSON;
- graph rendering;
- DOT and Mermaid output;
- deterministic node ordering.

### Commands

```bash
nxr task ci
nxr plan ci
nxr graph ci
nxr graph ci --format mermaid
```

### Exit criteria

- graph cycles report a complete cycle path;
- repeated dependencies execute once;
- plans are inspectable before execution;
- missing app/task references fail during planning.

## Phase 9: scheduler and parallel groups

### Deliverables

- async scheduler;
- serial and parallel execution;
- global job limit;
- group job limits;
- fail-fast;
- keep-going;
- dependent skipping;
- final status table;
- duration reporting.

### Commands

```bash
nxr task ci --jobs 4
nxr task dev --keep-going
```

### Exit criteria

- independent nodes can execute in parallel;
- dependents never run after failed prerequisites;
- scheduler shutdown leaves no owned processes;
- final exit status follows documented rules.

## Phase 10: process supervision

### Deliverables

- Unix process groups;
- graceful shutdown timeout;
- signal escalation;
- multi-child cleanup;
- focused interactive child behavior;
- terminal resize propagation;
- Windows supervision abstraction;
- process generation IDs.

### Exit criteria

- one Ctrl-C triggers graceful group shutdown;
- a repeated interrupt escalates;
- background descendants are not left behind;
- interactive foreground tasks remain usable.

## Phase 11: rich output pipeline

### Deliverables

- internal event bus;
- live labeled output;
- grouped output;
- failure-only output;
- summary mode;
- raw mode;
- JSON Lines events;
- log persistence;
- ANSI-aware rendering;
- terminal-width adaptation;
- non-TTY renderer.

### Commands

```bash
nxr task ci --output grouped
nxr task ci --output failures
nxr task ci --events jsonl
```

### Exit criteria

- parallel output is attributable to nodes;
- binary/raw output has an escape path;
- renderer backpressure does not deadlock children;
- event schema is versioned.

## Phase 12: watch and restart

### Deliverables

- native filesystem watcher;
- debounce;
- include/exclude patterns;
- restart-on-change;
- rerun-on-change;
- process-tree replacement;
- generation boundaries in logs;
- dependent subgraph invalidation;
- screen clearing options.

### Commands

```bash
nxr watch test
nxr watch dev
nxr task dev --watch
```

### Exit criteria

- atomic editor saves trigger one logical restart;
- old processes terminate before new generation starts;
- rapid changes coalesce;
- shutdown cleans watcher and all children.

## Phase 13: deep development-shell integration

### Deliverables

- `--shell <name>`;
- environment policy model;
- named shell discovery;
- integrated shell markers;
- automatic `nxr` installation in configured dev shells;
- Bash/Zsh/Fish shell hooks;
- direnv-friendly activation;
- completion cache configuration;
- nested-shell detection.

### Example flake configuration

```nix
perSystem.nxr = {
  shellIntegration.enable = true;
  shellIntegration.devShells = [ "default" "backend" ];

  tasks.integration = {
    app = "test-integration";
    environment = {
      policy = "devShell";
      shell = "backend";
    };
  };
};
```

### Exit criteria

- `use flake` can activate `nxr` completion automatically;
- no global dotfiles are changed;
- shell hook is idempotent;
- app execution outside a shell remains supported;
- selected shell behavior is visible in plans.

## Phase 14: argument and group semantics

### Deliverables

- task argument forwarding policies;
- named entrypoint forwarding;
- group aliases;
- category listing;
- optional argument descriptions;
- completion for documented arguments;
- parallel group stdin policy;
- interactive-child selection.

### Exit criteria

- multi-node argument ambiguity is rejected;
- apps remain authoritative for argument validation;
- completion metadata does not become a second parser;
- stdin ownership in parallel groups is deterministic.

## Phase 15: V2 stabilization

### Deliverables

- task schema V1 freeze;
- performance testing with large graphs;
- watcher stress tests;
- process cleanup stress tests;
- shell matrix tests;
- migration documentation;
- V2.0 release.

### V2.0 acceptance criteria

A project can define:

- ordinary standard apps;
- an optional DAG task;
- a parallel development group;
- watch/restart behavior;
- a named dev-shell execution policy;
- automatic session-local completion through direnv;

and run all of it with predictable signals, arguments, output, and exit status.

# V2.x — Stabilization and V3 bridge

V2.x releases harden the workflow layer before the repository model expands.

## Phase 16: extension and compatibility boundary

### Deliverables

- stable task schema V1;
- stable event schema V1;
- explicit extension points for metadata adapters;
- capability-negotiated Nix adapter;
- performance measurements for large task graphs;
- daemon feasibility prototype;
- persistent-run database feasibility prototype;
- adapters for reading mission-control and devenv metadata where practical;
- migration tooling from common just and mise task layouts;
- ADR completion through ADR-0120.

### Exit criteria

- V3 can build on V2 without changing ordinary app execution;
- extension APIs are narrow and versioned;
- no adapter creates a second authoritative operation definition;
- daemon use remains optional;
- current Nix workflows remain fully usable without V3 features.

# Long-term roadmap — V3 workspace control plane

V3 turns `nxr` from a workflow runner into a Nix-native workspace and CI control plane.

The core goal remains:

> Take the best command, task, environment, monorepo, CI, and remote-execution ideas from the broader ecosystem and rebuild them around the way developers already use flakes, apps, checks, development shells, Nix stores, remote builders, and direnv.

V3 must remain progressively adoptable. A repository with only standard apps remains valid.

# V3.0 — Monorepo intelligence

## Phase 17: first-class projects

### Deliverables

- versioned `projects` schema;
- canonical project identifiers;
- project roots, tags, owners, type, language, and supported systems;
- project-to-app associations;
- explicit project dependency declarations;
- project inspection and JSON output;
- project graph rendering.

### Example addresses

```text
//apps/web
//services/api
//crates/auth
//infra/production
```

### Commands

```bash
nxr projects
nxr project //services/api
nxr graph projects
nxr deps //services/api
nxr rdeps //crates/auth
```

### Exit criteria

- project identity is stable across machines;
- projects can be adopted incrementally;
- apps remain runnable without project metadata;
- graph errors identify exact project relationships.

## Phase 18: ecosystem graph adapters

### Deliverables

- Cargo workspace adapter;
- Node workspace adapter for common package managers;
- Go workspace/module adapter;
- Python package/workspace adapter;
- JVM module adapter where feasible;
- Terraform module discovery;
- generated-code and schema dependency hooks;
- explicit override and suppression mechanisms;
- confidence labels for inferred edges.

### Design rule

Adapters infer project relationships and suggested apps. They do not install language runtimes because Nix owns the toolchain.

### Exit criteria

- inferred relationships are inspectable;
- explicit metadata can override inference;
- low-confidence inference cannot silently affect destructive workflows;
- mixed-language repositories can share one graph.

## Phase 19: affected analysis and query language

### Deliverables

- Git diff integration;
- file-to-project ownership;
- upstream and downstream graph traversal;
- task/action invalidation;
- changed flake-input awareness;
- named scopes;
- project/task query language;
- affected graph visualization;
- explanation of why each node is affected.

### Commands

```bash
nxr affected test
nxr affected build --base origin/main
nxr query 'tag:backend & affected'
nxr explain-affected //services/api#test
```

### Exit criteria

- every affected result has an explanation path;
- changes to shared lock or generator inputs invalidate the correct graph region;
- users can override false-positive and false-negative mappings;
- affected execution works locally and in CI.

### V3.0 acceptance criteria

A complex mixed-language monorepo can model projects once, expose each project's flake apps, and run the minimum explainable set of operations for a change.

# V3.1 — Action contracts, artifacts, and caching

## Phase 20: declared actions

### Deliverables

- action as a distinct contract layered on an app;
- declared source, project, environment, and artifact inputs;
- declared outputs;
- execution properties;
- hermeticity classification;
- cacheability and remote-safety flags;
- action-plan inspection;
- promotion from mutable app to hermetic action.

### Execution tiers

```text
workspace  fast mutable execution against the checkout
hermetic   sandboxed execution with explicit inputs
remote     action executed on a compatible worker
```

### Exit criteria

- an ordinary app is never implicitly claimed to be hermetic;
- action identity includes all declared behavior-affecting inputs;
- missing declared inputs can be diagnosed in strict mode;
- hermetic actions can use native Nix derivations where appropriate.

## Phase 21: artifact model

### Deliverables

- artifact taxonomy;
- Nix-store artifact references;
- workspace artifact capture;
- test/report/log artifacts;
- local content-addressed store for non-Nix artifacts;
- artifact manifests;
- pull, inspect, diff, and replay commands;
- retention and garbage collection;
- checksums and optional signatures.

### Commands

```bash
nxr artifacts <run>
nxr artifact pull <digest>
nxr replay <run>
nxr diff-runs <old> <new>
```

### Boundary

The Nix store remains authoritative for Nix realizations. The `nxr` CAS exists only for workspace outputs, reports, logs, and cross-worker transport that the Nix store does not naturally represent.

## Phase 22: action cache

### Deliverables

- deterministic action hashing;
- local result cache;
- optional remote cache protocol;
- terminal/event replay;
- artifact restoration;
- cache explanation;
- read-only and read-write trust modes;
- cache poisoning defenses;
- branch and trust-domain isolation.

### Exit criteria

- every hit can explain its action identity;
- unsafe or undeclared operations are not cached by default;
- cache restore cannot overwrite paths outside declared outputs;
- remote cache is optional and self-hostable.

### V3.1 acceptance criteria

A declared action can run locally, be promoted to a hermetic Nix-backed execution where possible, and safely replay its non-Nix artifacts and structured results.

# V3.2 — CI control plane and test intelligence

## Phase 23: provider-independent CI plans

### Deliverables

- CI workflow schema built from the same task/action graph;
- pull-request, main, release, and scheduled workflow profiles;
- GitHub Actions adapter;
- generic bootstrap contract for other providers;
- provider-independent plan output;
- concurrency and cancellation policy;
- artifacts, reports, and annotations;
- trusted/untrusted execution classification.

### Intended CI shape

```yaml
steps:
  - checkout
  - install Nix and nxr
  - run: nxr ci run pull-request
```

The provider file bootstraps the graph; it does not redefine the graph.

### Exit criteria

- the local plan and CI plan are structurally comparable;
- provider-specific behavior is isolated in adapters;
- workflows can be simulated locally without secrets;
- CI failures link back to graph nodes and app definitions.

## Phase 24: dynamic scheduling and sharding

### Deliverables

- historical node durations;
- critical-path calculation;
- adaptive job counts;
- balanced test shards;
- automatic split of large suites where adapters support it;
- retry and timeout policies;
- scheduling simulation;
- resource class requirements.

### Commands

```bash
nxr ci plan pull-request
nxr test --shard auto
nxr ci explain-schedule <run>
```

### Exit criteria

- shards are stable enough to debug but adapt to measured durations;
- dependencies and artifacts are respected across shards;
- scheduler decisions are inspectable;
- static manual matrices remain available as a fallback.

## Phase 25: test intelligence

### Deliverables

- stable test identity;
- pass/fail and duration history;
- flaky-test classification;
- isolated retry policies;
- changed-test prioritization;
- test ownership;
- failure clustering;
- reproduction commands;
- test report ingestion adapters.

### Exit criteria

- retries never silently turn a failing workflow green without annotation;
- deterministic and likely-flaky failures are distinguished;
- every retry preserves the original failure;
- local developers can reproduce the exact shard and seed.

## Phase 26: public run event protocol

### Deliverables

- typed event schema;
- durable run IDs;
- event streaming;
- CI annotation consumer;
- dashboard consumer;
- OpenTelemetry mapping;
- test/report event types;
- artifact references;
- forward-compatible schema rules.

### V3.2 acceptance criteria

A repository can replace duplicated CI task YAML with one inspectable graph, run only affected work, shard it from historical data, and preserve structured events, artifacts, and reproduction information.

# V3.3 — Worker fabric and distributed execution

## Phase 27: optional `nxrd`

### Deliverables

- local workspace index;
- warm Nix evaluation cache;
- persistent graph state;
- run history;
- filesystem event service;
- process registry;
- local API;
- explicit direct-mode fallback;
- lifecycle and upgrade policy.

### Design rule

```bash
nxr test
```

must remain possible without a daemon.

The daemon accelerates and coordinates advanced features; it is not the basic trust anchor.

## Phase 28: worker protocol

### Deliverables

- worker registration;
- capability advertisement;
- system, CPU, memory, GPU, KVM, Xcode, and isolation capabilities;
- lease and heartbeat protocol;
- action assignment;
- log and event streaming;
- artifact transfer;
- cancellation;
- worker draining;
- protocol version negotiation.

### Worker examples

```text
local laptop
macOS build host
NixOS worker
Kubernetes worker
DevPod host
DevCell appliance
ephemeral CI runner
GPU machine
```

## Phase 29: Nix remote-builder integration

### Deliverables

- discovery and visualization of configured remote builders;
- routing of derivation work through native Nix;
- builder feature matching;
- distinction between Nix derivations and nxr workspace actions;
- unified run events across both execution mechanisms;
- fallback and failure diagnostics.

### Rule

Do not recreate Nix remote builds. Use them whenever the work is a derivation.

## Phase 30: distributed workspace actions

### Deliverables

- source snapshot or declared-input transfer;
- remote sandbox materialization;
- worker-side Nix closure realization;
- action execution;
- artifact and event return;
- trusted and untrusted pools;
- optional microVM/container isolation;
- burst mode across personal and organization workers.

### Commands

```bash
nxr test --remote
nxr test --burst
nxr workers
nxr workers explain-match //services/api#test
```

### V3.3 acceptance criteria

The same declared action can execute on a compatible local or remote worker, while native Nix derivations continue to use standard Nix distribution and caches.

# V3.4 — Development fabric

## Phase 31: services as first-class long-running apps

### Deliverables

- service metadata layered on apps;
- readiness and liveness probes;
- dependency states;
- restart policies;
- graceful ordered shutdown;
- log and event integration;
- persistent and ephemeral service classes;
- service status queries.

### Example

```text
postgres@ready
  → migrations@complete
    → api@ready
      → integration-tests
```

## Phase 32: local networking and service discovery

### Deliverables

- automatic port allocation;
- stable project-local service names;
- optional local DNS;
- optional local TLS;
- collision handling;
- exported connection metadata;
- links in terminal, TUI, and IDE consumers;
- per-workspace namespaces.

### Exit criteria

- two worktrees can run the same stack concurrently;
- allocated endpoints are discoverable without parsing logs;
- fixed-port applications have explicit collision behavior;
- TLS and DNS are optional.

## Phase 33: worktree and branch environments

### Deliverables

- workspace identity per checkout or worktree;
- isolated ports;
- isolated service state;
- environment snapshots;
- process namespaces;
- cache sharing policy;
- create/up/status/down/destroy lifecycle;
- safe stale-workspace cleanup.

### Commands

```bash
nxr workspace create feature-auth
nxr workspace up
nxr workspace status
nxr workspace destroy
```

## Phase 34: remote development backends

### Deliverables

- DevPod adapter;
- devcontainer adapter;
- DevCell adapter;
- generic SSH backend;
- environment plan export;
- local-to-remote command continuity;
- port and service forwarding;
- remote workspace lifecycle;
- editor connection metadata.

### V3.4 acceptance criteria

A branch can become a complete local or remote environment with isolated services, predictable endpoints, development-shell integration, and the same app/task commands used everywhere else.

# V3.5 — Platform interfaces, policy, and delivery

## Phase 35: TUI and web dashboard

### Deliverables

- project/task/service graph;
- running process view;
- live output;
- run history;
- artifacts;
- affected graph;
- worker status;
- CI critical path;
- test flakiness;
- replay and cancellation controls.

The TUI and web UI consume the same public event and query APIs as every other integration.

## Phase 36: IDE and agent protocol

### Deliverables

- nearest project/app resolution;
- list operations for current file;
- affected tests for current change;
- start required services;
- structured diagnostics;
- run and cancellation APIs;
- repository graph queries;
- stable machine-readable capability descriptions;
- VS Code reference integration;
- agent-oriented command protocol.

### Goal

An editor or coding agent should not guess repository commands from prose. It should query the repository's operational interface.

## Phase 37: policy, capabilities, and approvals

### Deliverables

- capability declarations;
- network, KVM, GPU, signing, cloud, and production access classes;
- trusted execution environments;
- secret references;
- approval gates;
- non-interactive authorization;
- organization policy layering;
- repository policy overrides;
- audit records.

### Security rule

Plans may contain:

```text
secret://provider/path
```

but never secret values.

## Phase 38: provenance and release orchestration

### Deliverables

- release workflow profiles;
- immutable artifact identity;
- SBOM references;
- signatures;
- build and test provenance;
- environment promotion;
- approval and deployment locks;
- rollback app coordination;
- post-deployment validation;
- integration with external deployment systems.

### Boundary

`nxr` coordinates build, approval, artifact promotion, and deployment apps.

It does not become the infrastructure reconciliation engine. Kubernetes, Terraform, Ansible, cloud APIs, and other deployment systems remain authoritative for their own state.

## Phase 39: ecosystem and extension model

### Deliverables

- versioned adapter SDK;
- project graph adapters;
- report parsers;
- CI provider adapters;
- worker backends;
- output/event consumers;
- extension conformance suite;
- compatibility policy;
- curated first-party integration set.

### V3.5 acceptance criteria

`nxr` exposes one local-first and self-hostable operational interface for developers, CI, remote workers, services, IDEs, agents, and release automation without replacing the Nix primitives underneath it.

# Long-term invariants

The following remain true through V3.5:

1. A standard flake app is always a valid leaf operation.
2. `nix run` remains a supported escape hatch.
3. Nix owns packages, runtime pinning, checks, store realizations, and native remote builds.
4. Development shells remain normal Nix outputs and integrate naturally with direnv.
5. Simple repositories do not need projects, actions, a daemon, a cache server, or workers.
6. Local and CI behavior derive from one inspectable graph.
7. Advanced metadata is versioned and additive.
8. Remote services are optional and self-hostable.
9. Secrets are referenced, never embedded in store paths, plans, or public metadata.
10. `nxr` coordinates deployment operations but does not own infrastructure state.
