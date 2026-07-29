# ADR-0171: Materialized process-compatible development environments

- **Status:** Proposed
- **Date:** 2026-07-28
- **Target release:** 3.4
- **Related ADRs:** ADR-0008, ADR-0113, ADR-0114, ADR-0128, ADR-0129, ADR-0157, ADR-0169, ADR-0170
- **Supersedes:** ADR-0130 (absolute ban on `print-dev-env` reconstruction)
- **Superseded by:** —

## Context

Shell-backed execution today commonly becomes:

```text
nix develop <flake>#<shell> -c nix run <flake>#<app>
```

Even with store-exe reuse, the outer `nix develop` keeps every shell-wrapped
invocation dependent on a Nix command when nothing about the shell changed.

ADR-0130 / [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md) correctly warned:
eagerly parsing `nix print-dev-env --json` and pretending to reproduce a full
interactive shell is environment-manager territory (direnv / devenv / HM).

The mise-like gap (ADR-0169 / 0170) needs a **narrower** claim: materialize a
**process environment** (variables + `PATH` entries) for direct spawn, with an
explicit fallback to `nix develop -c` when unsupported shell semantics are
required.

## Decision drivers

- Warm pre-child overhead should approach existing “tens of milliseconds” goals
  with **zero** Nix subprocesses when a compatible snapshot is warm.
- Never silently approximate interactive shell features.
- direnv remains activation authority for interactive shells (ADR-0128).
- Secrets must never enter environment snapshot caches, nxrd, plans, events, or
  cache keys.
- Daemon absence must preserve standalone CLI behavior (ADR-0157).

## Considered options

### Option A — Keep develop-wrap only (ADR-0130 absolute)

Correct but leaves shell-backed scripts/apps slow forever.

### Option B — Always reconstruct full shells in-process

Rejected: hooks, functions, aliases, traps cannot be faithfully reproduced.

### Option C — Process-compatible snapshots + explicit fallback (chosen)

Use `nix print-dev-env` (feature-detected) to build a normalized snapshot for
direct spawn; fall back to `nix develop -c` when the shell is not
process-representable or the user forces exact shell semantics.

## Decision

### Normalized object

```text
DevEnvironmentSnapshot {
  flake_identity
  system
  shell
  nix_identity
  variables            # exported process env
  path_entries
  unsupported_features # why fallback may be required
  fingerprints
  protocol_version
}
```

### Execution policies

Extend the existing `smart | always | never` shell vocabulary rather than
inventing a second flag family where possible:

| Intent | Behavior |
|---|---|
| process / smart (default when representable) | Materialize exported variables and spawn directly |
| exact shell (`--shell-mode always` or future explicit mode) | `nix develop -c …` |
| never | No shell snapshot; caller env only |

Recommended spawn hierarchy for a workspace script / live file-backed app with
a requested shell:

1. Matching dev shell already active (`NXR_DEV_SHELL`) → direct spawn with
   inherited environment (0 Nix).
2. Warm cached process-compatible snapshot → merge + direct spawn (0 Nix).
3. Cold process-compatible shell → `print-dev-env`, normalize, cache, spawn
   (one env realization).
4. Unsupported semantics → `nix develop -c` script/app.
5. No shell requested → direct spawn in caller environment.

### Contract: process environment ≠ interactive shell

The fast path does **not** reproduce:

- shell functions / aliases / traps;
- interactive prompt initialization;
- arbitrary shell-hook side effects;
- shell-specific arrays / internals.

When those are required, NXR falls back to `nix develop -c` rather than partial
emulation.

### Environment merge order

```text
caller environment or clean-environment base
  → Nix development-environment snapshot
  → named NXR execution-context set/unset
  → CLI overrides
  → resolved secrets (immediately before spawn only)
```

### Cache identity

Include: canonical flake identity, system, devShell name, Nix executable
identity/version, Nix configuration fingerprint, flake.lock digest, relevant
Nix source fingerprint, shell environment protocol version, requested
environment mode.

Exclude: complete caller environment, secret values, script contents, CWD
(unless the shell definition actually depends on it).

**Script contents and shell environment are separate invalidation domains.**

### nxrd (optional)

Add non-authoritative `dev_env.get` / `dev_env.put` / `dev_env.invalidate`
retaining the same disk-cache + standalone fallback rules as other caches.

Disk persistence is **opt-in** via `NXR_DEV_ENV_CACHE=on` (unset = disabled) until
secret provenance is complete; both paths reject non-placeholder secret values.

## Public contract

- Feature-detect `nix print-dev-env` / JSON; degrade to develop-wrap when
  unavailable.
- Plans/explain must state `environment_mode: process | shell` and fallback
  reasons.
- No claim of full shell equivalence in docs or doctor output.

## Consequences

### Positive

- Mise-like warm path after one environment realization.
- Honest fallback preserves correctness.
- Aligns with one-shell DAG optimization goals (ADR-0129) without requiring
  interactive shell fidelity.

### Negative

- Two semantic modes users must understand (`process` vs exact `shell`).
- Experimental Nix interface requires capability detection and tests across
  Nix versions.

### Neutral or accepted tradeoffs

- Interactive developers should still use direnv / `nix develop` for a real
  shell; NXR snapshots are for **running operations**, not replacing the
  shell.

## Compatibility and migration

- Default behavior for ordinary apps without opt-in fast path remains develop
  wrap / store-exe as today until explicitly routed through process mode.
- ADR-0130’s warning stands against **faithful shell reconstruction**; this ADR
  authorizes **process-env materialization only**.

## Security and trust

- Snapshot cache must never persist secret values or secret material.
- Dev-environment disk cache is **opt-in** (`NXR_DEV_ENV_CACHE=on`) until secret
  provenance is complete; callers sanitize known secret names before persistence.
- Cache keys never include secret values.
- Untrusted flake metadata sanitized before terminal rendering (existing rule).

## Operational impact

- Disk cache under existing NXR cache roots; bounded nxrd memory optional.
- Invalidation on flake.lock / Nix config / shell source / protocol version
  changes—not on script body edits.

## Validation plan

| Scenario | Nix subprocess expectation |
|---|---|
| Matching active shell | 0 |
| Warm materialized environment | 0 |
| Cold materialized environment | One env realization path |
| Forced exact shell | Current `nix develop -c` |
| No shell requested | 0 |

Also: unsupported constructs → explicit fallback; secrets absent from snapshot
files; perf counters for warm/cold paths; regression fixtures.

## Rollout

Architecturally after ADR-0169 (script addressability). May ship as 3.4 even if
3.3 already includes scripts with develop-wrap only.

## Unresolved questions

- Exact CLI surface for forcing process vs shell if `smart|always|never` is
  insufficient.
- Which `print-dev-env` fields are classified as unsupported vs representable.
- Interaction with clean-env and context `keep` lists at the field level.

## References

- [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md) § one-shell optimization
- [DEV_ENV_INTEGRATION.md](../DEV_ENV_INTEGRATION.md)
- [PERFORMANCE.md](../PERFORMANCE.md)
- ADR-0128 direnv authority
- ADR-0157 optional nxrd
