# ADR-0170: File-backed `nxr.apps` and live-workspace fast path

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** 3.3
- **Related ADRs:** ADR-0001, ADR-0010, ADR-0016, ADR-0134, ADR-0169, ADR-0171
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR-0169 adds disposable workspace scripts. Once a script becomes a stable
project operation, authors should promote it to a standard flake app **without
copying the script body into `flake.nix`**.

Today `nxr.apps` only supports inline `script = "…"` bodies emitted via
`writeShellApplication`. That forces either duplication or staying on
`nxr script` forever.

## Decision drivers

- Standard `apps.<system>.<name>` remains the portable leaf (ADR-0001 / 0010).
- `nix run .#<name>` must work without NXR, nxrd, or an active shell.
- Live checkout edits should be usable locally without waiting on store rebuild
  when the author opts in.
- Discovery/invalidation must notice non-Nix script files (ADR-0134 /
  `discoveryInputs`).

## Considered options

### Option A — Only inline `script` (status quo)

Simple, but poor promotion path from workspace scripts.

### Option B — File-backed apps + optional local fast path (chosen)

Mutually exclusive `script` vs `file` on `nxr.apps`; always emit a store-backed
app; optionally advertise live-workspace metadata for local NXR runs.

### Option C — Fast path only (no store app)

Rejected: breaks `nix run` escape hatch and remote flake usability.

## Decision

### Module shape

Extend `perSystem.nxr.apps.<name>` so `script` and `file` are mutually
exclusive:

```nix
nxr.apps.deploy = {
  description = "Deploy the application";
  file = "scripts/deploy.nu";
  interpreter = "${pkgs.nushell}/bin/nu"; # optional when shebang suffices
  runtimeInputs = opsTools;
  fastPath = {
    enable = true;
    shell = "ops"; # optional named devShell for ADR-0171 / develop wrap
  };
};
```

Rules:

- `file` is a **repository-relative** string (no absolute paths, no `..`).
- The module still emits `apps.<system>.<name>` as a store-backed wrapper with
  pinned `runtimeInputs` (and copies or references the file into the
  derivation as today for hermetic `nix run`).
- Register `file` in discovery / execution invalidation inputs automatically.

### Optional local fast-path metadata

For a **local** checkout, NXR may discover metadata equivalent to:

```json
{
  "name": "deploy",
  "workspace_path": "scripts/deploy.nu",
  "interpreter": "/nix/store/...-nushell/bin/nu",
  "shell": "ops",
  "fallback_app": "deploy"
}
```

Then `nxr deploy` may execute the live workspace file when
`fastPath.enable = true`. Otherwise it uses the standard app.

Remote flakes and non-local refs **must not** select the live-workspace path.

### Plan honesty

Plans / `nxr explain` must show when the fast path was chosen:

```text
operation: workspace-script
path: …/scripts/deploy.nu
environment: devShells.<system>.ops
fallback: apps.<system>.deploy
mutable_source: true
```

or why it was not (remote flake, `fastPath.enable = false`, missing file,
`--offline` policy, etc.).

## Public contract

- `script` XOR `file` on `nxr.apps`.
- Store app always generated for `nix run` compatibility.
- `fastPath.enable` default: `false` (opt-in mutable local path).
- Bare `nxr <app>` may use live file only when metadata opts in; never confuse
  with ADR-0169’s `.nxr/scripts` name lookup unless paths coincide by author
  choice.

## Consequences

### Positive

- Promote scripts without body duplication.
- Preserves portable leaves.
- Makes mutable vs store execution visible.

### Negative

- Two execution paths for the same name; explainability is mandatory.
- Authors must keep `file` paths valid for both store packaging and live runs.

### Neutral or accepted tradeoffs

- Warmth of the surrounding shell remains ADR-0171’s concern.
- Task schema still requires `app:` string; no leaf union yet.

## Compatibility and migration

- Existing `script =` apps unchanged.
- Inline → file is a pure authoring move; no runtime schema major.
- Helpers (`mkApp`) may gain a file-oriented variant later; not required for
  acceptance if the flake-parts module covers the path.

## Security and trust

- Reject absolute / `..` `file` values at evaluation or discovery time.
- Secrets still never appear in fast-path metadata, plans, or events.
- Live path inherits project trust / confirmation rules like other workspace
  actions.

## Operational impact

- Script content changes must invalidate store-exe / discovery caches that
  depended on the packaged file; live path does not require shell snapshot
  invalidation (ADR-0171 separates those domains).

## Validation plan

- Fixture app with `file =`; assert `nix run` and `nxr` both work.
- Edit workspace file with `fastPath.enable = true`; `nxr <app>` picks up
  change without rebuild; `nix run` still uses store copy until rebuild.
- Remote flake ref never selects live path.
- `nxr explain` surfaces fast-path decision reasons.

## Rollout

Ship with or immediately after ADR-0169. Materialized environments (ADR-0171)
accelerate shell-backed fast paths but are not required for file-backed store
apps.

## Unresolved questions

- Exact JSON/attr shape for fast-path metadata (`nxrMetadata` vs task document).
- Whether `interpreter` is required when the store wrapper uses
  `writeShellApplication` around `exec`.

## References

- [APP_AUTHORING.md](../APP_AUTHORING.md)
- `nix/modules/apps.nix`
- ADR-0169 workspace scripts
- ADR-0134 discovery inputs
