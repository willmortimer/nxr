# Ecosystem Synthesis

## 1. Core product goal

`nxr` should take the best ideas from:

- Nix flakes, packages, apps, checks, development shells, stores, and remote builders;
- direnv and nix-direnv;
- mise tasks;
- just;
- flake-parts mission-control;
- devenv tasks and processes;
- Taskfile and Make-style command discovery;
- Nx, moonrepo, Turborepo, Pants, and Bazel-style monorepo orchestration;
- process-compose, Foreman, and Compose-style service supervision;
- modern CI systems and remote execution protocols;

and combine them into a workflow designed for developers who already use Nix.

The objective is not to place a generic task runner beside Nix. The objective is to make Nix's existing project primitives feel like a complete development and automation system.

The canonical progression is:

```text
flake app
  → ergonomic command
  → task graph node
  → monorepo action
  → CI or remote execution unit
```

At every stage, ordinary Nix commands remain available.

## 2. Selection rule

`nxr` should inherit a feature only when it can satisfy all of the following:

1. It improves the daily development workflow.
2. It can be expressed without weakening Nix reproducibility.
3. It composes with existing flakes, apps, checks, and development shells.
4. It does not require a second package or runtime pinning system.
5. It can remain optional for simple projects.
6. It has a clear local and CI behavior.
7. It can be inspected through a stable plan or machine-readable interface.
8. It does not trap the project behind `nxr`.

The design should reject features that are attractive in isolation but create a duplicate source of truth.

## 3. What to inherit from Nix

### Flakes and lock files

Use flakes as the project boundary and `flake.lock` as the canonical external-input lock.

Do not add an independent toolchain lock format.

### Apps

Use standard flake apps as the canonical executable leaf operations.

An operation such as:

```text
test
lint
dev
deploy-staging
```

should normally exist as an app before it exists as a task node.

### Packages and the store

Use packages and store paths for exact executable dependencies and immutable artifacts.

Do not reimplement package installation.

### Checks

Use checks for hermetic, cacheable validation.

Keep a visible distinction between:

```text
app    mutable developer operation
check  sandboxed Nix validation
```

### Development shells

Use development shells for interactive environment composition:

- editor tools;
- language servers;
- debuggers;
- prompt and completion integration;
- local environment variables;
- optional services;
- the `nxr` binary itself.

Do not make development-shell entry a hidden prerequisite for every app.

### Remote builders and substituters

Use native Nix distribution when an operation is a derivation.

Add an `nxr` worker protocol only for workspace-style actions that cannot naturally be represented as derivations.

## 4. What to inherit from direnv and nix-direnv

- automatic activation when entering a repository;
- fast re-entry through cached shell environments;
- invalidation when relevant flake files change;
- session-local environment changes;
- compatibility with nested project directories;
- no mutation of global shell configuration.

The intended experience is:

```bash
cd project
# direnv activates the dev shell
nxr <TAB>
nxr test
```

A configured development shell should be able to install `nxr` and activate its completion integration automatically.

`nxr` must still function when direnv is absent.

## 5. What to inherit from just

- extremely small and memorable command surface;
- upward project-root discovery;
- excellent direct argument passing;
- clear recipe descriptions;
- useful listing and shell completion;
- static detection of unknown names and dependency cycles;
- support for commands implemented in arbitrary languages;
- explicit working-directory behavior;
- low ceremony for small projects.

Do not inherit a separate recipe language as the required source of truth.

The equivalent of a just recipe should normally be a standard flake app or a V2 task that coordinates apps.

## 6. What to inherit from mise tasks

- descriptions, aliases, categories, and hidden tasks;
- dependency DAGs and parallel scheduling;
- serial and parallel groups;
- source and output declarations;
- freshness explanations;
- watch mode;
- task validation;
- generated task documentation;
- monorepo-aware task addressing;
- configurable output modes;
- confirmations and timeouts;
- interactive-task handling;
- dry-run and dependency visualization.

Do not inherit mise's runtime installation role. Nix owns tools, versions, and execution closures.

Do not require `mise.toml` beside `flake.nix`.

## 7. What to inherit from mission-control

- a Nix-native command catalog;
- command descriptions and categories;
- commands made available through a development shell;
- a useful shell-entry message;
- project-root awareness;
- a tiny wrapper command;
- flake-parts integration.

`nxr` generalizes this model by making standard flake apps canonical and the development shell optional.

Mission-control-style shell UX should become one presentation mode, not the execution foundation.

## 8. What to inherit from devenv

- tasks and long-running processes in one dependency model;
- dependency states such as completion and readiness;
- status checks that skip unnecessary setup;
- process readiness and restart policies;
- service integration;
- process/task introspection;
- watch behavior;
- a programmatic protocol for external consumers.

Do not require replacing an existing flake architecture with a separate environment framework.

`nxr` should interoperate with devenv projects and consider adapters where they preserve normal flake outputs.

## 9. What to inherit from Taskfile and Make

### Taskfile

- portable single-binary execution;
- simple task descriptions;
- source-aware watch behavior;
- explicit status and skip conditions;
- namespaces and included task definitions;
- straightforward CI use.

### Make

- universal target vocabulary;
- composability;
- a low barrier to understanding `build`, `test`, and `clean`;
- widespread expectation that a project exposes named operations.

Do not inherit Make's file-timestamp semantics as the default execution model, shell portability problems, or implicit-rule complexity.

## 10. What to inherit from Nx, moonrepo, and Turborepo

- an explicit project graph;
- a task graph derived from project relationships;
- affected-project and affected-task analysis;
- project and task filtering;
- smart input hashing;
- local and remote result caching;
- replay of artifacts and logs;
- CI graph pruning;
- dynamic distribution across workers;
- historical-duration-based scheduling;
- task and project visualization;
- incremental adoption;
- language-ecosystem adapters.

Nix provides a stronger base for toolchain and executable identity. `nxr` should not duplicate the integrated tool installers common in language-focused monorepo systems.

## 11. What to inherit from Bazel and Pants

- hermetic action contracts;
- declared inputs, outputs, and execution properties;
- dependency inference where reliable;
- content-addressed remote caching;
- capability-aware remote execution;
- a queryable graph;
- structured build and test events;
- reproducible execution plans;
- a strong distinction between analysis and execution.

Do not require users to rewrite ordinary language projects into a new build language before gaining value.

The ambitious path is gradual promotion:

```text
ordinary app
  → app with metadata
  → task node
  → declared action
  → remotely executable action
```

## 12. What to inherit from process supervisors

From process-compose, Foreman, Compose-style systems, and development service managers:

- parallel process groups;
- labeled logs;
- readiness and health checks;
- restart policies;
- graceful shutdown;
- dependency ordering;
- environment and port management;
- status inspection;
- foreground process selection.

The supervised process should still be a flake app whenever practical.

## 13. What to inherit from modern CI

- provider-independent execution plans;
- annotations and test reports;
- artifacts and provenance;
- dynamic matrices and sharding;
- retry policy;
- concurrency controls;
- approval gates;
- trusted and untrusted execution pools;
- historical timing and flakiness analysis.

CI configuration should become a thin bootstrap for the same graph developers run locally.

## 14. Progressive product layers

### V1: command layer

```text
standard flake apps
+ discovery
+ completion
+ exact execution
+ diagnostics
```

Comparable daily ergonomics to the best small command runners without another manifest.

### V2: workflow layer

```text
apps
+ DAG tasks
+ groups
+ watch
+ process supervision
+ development-shell integration
+ structured output
```

Comparable orchestration to sophisticated task runners while preserving the Nix substrate.

### V3: workspace control plane

```text
projects
+ affected analysis
+ action contracts
+ cache and artifacts
+ CI planning
+ remote workers
+ service fabric
+ IDE and agent protocols
```

Comparable monorepo and CI capability to the strongest dedicated platforms, but designed around Nix primitives and open local-first infrastructure.

## 15. Deliberate non-goals

`nxr` must not become:

- another language runtime installer;
- another package manager;
- a mandatory replacement for `nix run`;
- a proprietary cloud requirement;
- a JavaScript-only monorepo system;
- a new Nix evaluator;
- a secrets vault;
- a container runtime;
- a Kubernetes replacement;
- an infrastructure state engine;
- a task DSL so powerful that apps become incidental.

The system wins only if it makes the way Nix developers already work more coherent.
