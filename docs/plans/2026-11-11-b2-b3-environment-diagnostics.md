# Sprint — B2 environment and B3 diagnostics

**Status:** implement only after the A4.2 score is frozen. Do not change agent-facing tools while that score is being recorded. No embedding model.

`nxr env explain` still reports a resolved compiler of `unknown` when it has only inspected ambient `PATH`.

## Order

B2.0 and B3.0 first, in parallel. Then attach cache decisions and diagnostics to that event stream. B1.1 ranking changes wait for the trials that selected the wrong operation.

## B2.0 — Verified toolchain

Static resolution does not enter a shell. It may report the declared shell, the declared `RUSTC_WRAPPER`, the working directory, and named lockfile paths. It must say the compiler was not verified.

Verified resolution may enter or realize the development shell. It then reports the shell that will run, the `rustc` and Cargo binaries with versions and store paths, the wrapper and sccache configuration, the target directory, the linker, and whether that context is already warm.

Qualification: the reported `rustc`, Cargo, and wrapper paths match the process that actually runs.

## B2.1 — Context identity

Fingerprint the execution context separately from the source fingerprint. These are different events:

- Source changed, toolchain identical.
- Toolchain changed.
- Compiler wrapper changed.
- Target directory changed.
- Development shell became stale.
- A remote worker has a compatible context that is not the same context.

Qualification: a source edit and a toolchain change produce two different explanations.

## B2.2 — Cache misses

Explain a miss from the same identities the cache uses. Cover a cold run, a warm run, a dirty source tree, and a toolchain change. The explanation names the invalidation cause.

## B3.0 — One event stream

The task scheduler and the supervisor append one versioned stream: run id, sequence, timestamp, node, payload. The CLI and MCP read that stream.

## B3.1 — Failure summary

`runs_get_diagnostics` names the failed node, the most useful diagnostic, and whether the failure looks like source, environment, infrastructure, or policy. An agent can identify that node without reading the whole log.

## Post-tag sprint

Start B2.0 and B3.0 together only after `v3.9.1` is tagged on `db8961a5f3b96e3e3ea41c514f5f233934951b15`. Do not start them during A4.2. v0.2.14 and distributed DAG stay out. Cache explanations, parsed diagnostics, SourceSet, and one remote nixdev attempt stay later slices. No embeddings.

B2.0 enters the declared shell and writes compiler, Cargo, wrapper, and target directory onto the shared receipt. It must say when those fields were not entered. B3.0 appends the existing supervisor events to that same receipt and serves them from the CLI and MCP. The CLI, MCP, and Lattice's harness read that one receipt.

## B2.0 and B3.0 implementation slice

Coding starts only after the `v3.9.1` tag. Do not start it during A4.2.

B2.0 enters the declared shell and writes compiler, Cargo, wrapper, and target directory onto one shared receipt. It must say when those fields were not entered. B3.0 appends the existing supervisor events to that same receipt and serves them from the CLI and `runs_get_events`. Parsed diagnostics and cache explanations stay in B3.1 and B2.2. No embeddings.

## Execution evidence

B2 and B3 write one versioned receipt. B2 contributes the verified shell, compiler, wrapper, target directory, and cache identity. B3 contributes the run events and the parsed Rust, Nix, and test diagnostics. The CLI, MCP, and Lattice's harness read that receipt. They do not keep a second log parser. Implementation still waits for the frozen A4 score. B2.0 and B3.0 remain the first slices. No embedding model.

## B3.2 — Artifacts and reruns

`runs_get_artifacts` returns digest-checked outputs. A rerun keeps the same provenance. An infrastructure failure is not rerun as if it were a compiler error.
