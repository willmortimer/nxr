# B2 and B3 — Environment resolution and structured diagnostics

**Status:** design. Do not implement in the A4 qualification slice. `nxr env explain` still reports a resolved compiler of `unknown` when it has only inspected ambient `PATH`.

## B2.0 — Environment resolution

`nxr env explain` answers two different questions, and it must say which one it answered.

Static resolution does not enter a shell. It reports the declared shell, the declared `RUSTC_WRAPPER`, the operation's working directory, and the lockfile paths named by the task. Compiler, Cargo, store paths, target directory, and linker stay unknown until they are verified.

Verified resolution may enter or realize the development shell. It then reports:

- The Nix development shell or execution context that will run.
- The Rust compiler and Cargo binaries, with versions and store paths.
- The `RUSTC_WRAPPER` and sccache configuration that apply.
- The target directory and linker configuration.
- The lockfile revisions and environment inputs that define the context.
- Whether that context is already warm and reusable.

The tool must not say the effective environment was verified when it only inspected ambient `PATH`.

## B2.1 — Environment fingerprint

An execution-context identity covers the environment inputs and the resolved toolchain. It distinguishes:

- Source changed, toolchain identical.
- Toolchain changed.
- Compiler wrapper changed.
- Build target directory changed.
- Development shell became stale.
- A remote worker has a compatible context that is not the same context.

That identity feeds cache reuse and execution planning. It is not only a diagnostic field.

## B2.2 — Cache explanation

An agent can ask why a task is recompiling. The report names the operation, the environment, and the identities the cache itself used:

- Compiler matched or differed.
- Target directory changed or stayed.
- Compiler cache was available or absent.
- Which input changed, in the same terms the cache key uses.

A cold run, a warm run, a dirty source tree, and a toolchain change are separate cases. The invalidation reason has to match the case. The qualification is those four cases, plus a count of compiler launches that the explanation says were unnecessary.

## B3 — Events, diagnostics, and artifacts

Bounded logs stay. Agents also need the execution events the scheduler and supervisor already produce.

```text
task scheduler and execution owner
        |
        v
versioned run-event stream
  run id, sequence, timestamp, node, payload
        |
        +-- CLI and TUI
        +-- Cursor MCP
        +-- Lattice agent
```

New MCP reads, after this design is implemented:

- `runs_get_events` returns the versioned stream for one run id.
- `runs_get_diagnostics` returns a short summary: which node failed, the most useful diagnostic, and whether the failure looks like source, environment, infrastructure, or policy.
- `runs_get_artifacts` returns output identities for that run.

The first diagnostics are Rust compiler JSON, Cargo test failures and exit codes, Nix evaluation and build failures, task-graph node failures, and cache hit, miss, and invalidation causes.

An infrastructure failure is not rerun as if it were a compiler error. `runs_rerun_failed` waits until the summary can tell those cases apart.

Qualification: an agent diagnoses and repairs a deliberate Rust failure from the structured diagnostic, without reading the whole build log and without starting a different task. The debugger and the agent read the same events.
