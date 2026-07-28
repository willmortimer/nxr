# ADR-0169: Workspace script execution (`nxr script`)

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** 3.3
- **Related ADRs:** ADR-0001, ADR-0002, ADR-0010, ADR-0012, ADR-0113, ADR-0147, ADR-0170, ADR-0171
- **Supersedes:** —
- **Superseded by:** —

## Context

NXR already solves second-order automation problems (prepared plans, store-exe
cache, workspace CAS, process supervision, optional `nxrd`). The glaring
ergonomic hole is first-order:

> Run this small checked-in script immediately in my project environment.

Today every leaf must be a flake app. Inline bodies go through `nxr.apps` /
`mkScriptApp`, and shell-backed runs often wrap `nix develop -c nix run`. That
is correct for stable packaged operations, but heavier than mise or Just when
the operation is literally a five-line checked-in script.

Changing the task schema so `app` becomes a union of app / file / shell / WASI
would force a schema-major decision before the simple workflow is validated.
Schema v2 also rejects unknown execution-affecting fields.

## Decision drivers

- Preserve bare `nxr <name>` as app-only (and existing task resolution).
- Do not introduce a second mandatory project manifest (ADR-0002).
- Reuse existing process, argument, signal, CWD, and context machinery.
- Keep workspace scripts clearly in the mutable / local-only tier (ADR-0147).
- Validate mise-like immediacy before expanding the task IR (V4).

## Considered options

### Option A — Automatic script tasks / loose task leaf union

Teach `tasks.*.app` (or a sibling field) to accept files, shell text, and
adapters immediately.

Advantages: one vocabulary for everything.

Disadvantages: schema-major before validation; pollutes name resolution;
risks silent field ignorance under v2 policy pressure.

### Option B — Explicit `nxr script` command (chosen)

Add a reserved command that runs a workspace script without changing task or
bare-app semantics.

Advantages: narrow; testable; preserves ADR-0001; no schema churn.

Disadvantages: two entry points until file-backed apps promote scripts (ADR-0170).

### Option C — New `.nxr.yaml` / TOML task manifest

Rejected: contradicts ADR-0002 and the Nix-native contract.

## Decision

Ship **workspace-script execution** as a first-class reserved command:

```bash
nxr script <PATH_OR_NAME> [--] [args...]
nxr in <shell> script <PATH_OR_NAME> [--] [args...]
```

### Lookup (deliberately narrow)

1. If the argument is an exact path (contains `/` or is `.` / `..`-relative),
   resolve it relative to the invocation CWD (or `-C` when set for the child).
2. Otherwise resolve `.nxr/scripts/<name>` under the discovered flake root
   (try common extensions only if `<name>` has no extension: prefer exact
   match first; then documented extension order if needed).
3. Accept executable files or files with a valid shebang.
4. Do **not** search `scripts/`, `bin/`, or arbitrary trees.

### Local-only contract

A workspace script:

- operates against the current checkout;
- is not remotely addressable by flake reference;
- is not implicitly hermetic;
- is not automatically result-cached;
- is not equivalent to a Nix derivation;
- still runs through NXR’s argument vector, environment policy, signals,
  working directory, confirmation, and process model.

### Name-resolution invariant

```bash
nxr test
```

continues to mean a flake app (or task under existing V2 rules)—never “whichever
app, task, or local script happened to win.” Scripts are only reached via
`nxr script …` until a file-backed app opts into a live fast path (ADR-0170).

### Progression (product)

```text
nxr script ./scripts/deploy.nu     # disposable local script
nxr script deploy                  # .nxr/scripts convention
nxr deploy                         # promoted flake app (later / ADR-0170)
nix run .#deploy                   # escape hatch
```

## Public contract

- New reserved top-level command: `script`.
- Convention directory: `.nxr/scripts/` (optional; not a task manifest).
- Exact argv, exit status, stdin/stdout/stderr, Ctrl-C, terminal resize, and
  CWD semantics match ADR-0012 / existing app foreground execution.
- Plans/events label the operation as a workspace script (mutable source),
  never as a store app, when that path is used.
- Out of scope for this ADR: task schema leaf unions, WASI, prompt protocols,
  YAML/TOML manifests, automatic script discovery across the tree.

## Consequences

### Positive

- Mise-like authoring without abandoning Nix leaves.
- Validates the workspace-script tier without a schema major.
- Clear promotion path into ADR-0170 / ADR-0171.

### Negative

- Another reserved verb; docs and completion must teach `script` vs bare app.
- Scripts outside `.nxr/scripts` require an explicit path.

### Neutral or accepted tradeoffs

- Cold shell-wrapped runs may still use `nix develop` until ADR-0171.
- No automatic caching of script results in 3.3.

## Compatibility and migration

- Existing apps, tasks, and `nix run` behavior unchanged.
- `nxr migrate mise|just` may later emit `.nxr/scripts` stubs or file-backed
  apps; not required for 3.3 acceptance.
- Remote flakes: `nxr script` requires a local checkout; remote refs error
  clearly.

## Security and trust

- Script path must reject `..` traversal outside the intended root when using
  the convention name form; path form follows normal filesystem permissions.
- Secrets never enter plans/events; inject only at spawn (existing policy).
- Do not execute non-shebang, non-executable files by shell-wrapping their
  contents unless an explicit interpreter policy is added later.

## Operational impact

- No new daemon requirement.
- No new persistent cache keyed by script body in 3.3.
- Reuse `PreparedTaskNode`-style planning fields: program, argv, cwd, env,
  timeout, termination grace, context, confirmation.

## Validation plan

- Fixture: `.nxr/scripts/hello.sh` and path form with multi-word args.
- Preserve exit status, signals, and streams (CLI integration tests).
- Confirm bare `nxr <name>` does not resolve to `.nxr/scripts/<name>`.
- Remote flake + `script` → hard error.
- `--offline`, clean env, contexts, confirm, `-C` behave like app runs.

## Rollout

1. CLI + planning path only (caller env / existing shell wrap).
2. Convention directory + completion.
3. Explain/plan visibility for workspace-script operations.
4. ADR-0170 / ADR-0171 as follow-on commits (may ship in the same minor line).

## Unresolved questions

- Extension search order for bare names under `.nxr/scripts`.
- Whether `nxr script --list` is worth a follow-up (not required for acceptance).

## References

- [ROADMAP.md](../ROADMAP.md) §3.3
- [MIGRATE_FROM_MISE_JUST.md](../MIGRATE_FROM_MISE_JUST.md)
- [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md)
- ADR-0147 two-tier actions
