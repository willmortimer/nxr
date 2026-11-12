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

## B3.2 — Artifacts and reruns

`runs_get_artifacts` returns digest-checked outputs. A rerun keeps the same provenance. An infrastructure failure is not rerun as if it were a compiler error.
