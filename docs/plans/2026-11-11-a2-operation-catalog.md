# A2 — Unified operation catalog

**Status:** ticket. Do not implement until `v0.2.14` has a stable identity. This does not change the execution protocol.

The catalog is derived from declarations NXR already loads. There is no second hand-maintained catalog.

## Sources

- Apps and tasks from `discover_catalog` in `crates/nxr-cli/src/commands/list.rs` (`CatalogEntries`).
- Scripts from `crates/nxr-cli/src/commands/script.rs`.
- Workflows from `crates/nxr-task/src/workflows.rs` (`.nxr/workflows.toml`).
- Processes from `crates/nxr-task/src/process.rs`.

Optional annotations may add a human description and a policy class. They do not redefine the command, the inputs, or the environment.

## Proposed CLI

These commands do not exist yet:

```text
nxr op search "test changed Rust packages"
nxr op describe <id> --json
nxr op plan <id> --json
nxr env explain <id>
```

## Operation record

Each record has a stable id, the source declaration, parameters, applicable platforms, an effect class, and a plan. The plan names the dev shell, the compiler, the working directory, the resolved inputs, and whether the task reuses that environment. Secret fields are references. Resolved secret values are not part of the record, the plan, or the explain output.

Search is deterministic text and alias matching. Semantic search and generated shell commands are out of scope.

## Acceptance

One query returns the same operation whether it was declared as a task, an app, a script, or a workflow. `nxr op plan` and `nxr env explain` name the environment that will actually run it. A missing declaration is an error, not a guessed command.
