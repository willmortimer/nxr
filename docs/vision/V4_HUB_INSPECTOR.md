# V4 Hub inspector integration (scaffolding)

This note describes how NXR's operation IR and durable run records connect to a
future NixPlane Hub inspector **without** importing fleet Assignment types into
NXR ([ADR-0174](../adr/0174-nixplane-fleet-state-boundary.md)).

## What exists today

| Piece | Location | Role |
| --- | --- | --- |
| Operation IR envelope | `crates/nxr-core/src/operation_ir.rs` | Versioned `Operation` JSON for wire/storage |
| Durable run metadata | `crates/nxr-core/src/durable_run.rs` | Pending/running run records + monotonic `event_sequence` |
| Hub preview hook | `durable_run::hub_inspector_operation_preview` | Lossless **operation kind + run id** JSON for inspector panels |
| Session supervisor | `execution_owner` + `nxrd` `run.*` | Live `nxr session` ownership; separate from durable runs |

Hub UI remains NixPlane-owned ([NixPlane ADR-0013](https://github.com/willmortimer/NixPlane/blob/main/docs/adr/0013-hub-web-ui-and-local-enroll-ui.md)).
NXR exposes structured run/operation previews only; it does not store Profile
Assignments or PhysicalHost inventory.

## Intended inspector flow (later)

1. Operator starts a durable run (`nxr runs` family — not shipped yet).
2. NXR persists `DurableRunRecord` under the user state dir (`$XDG_STATE_HOME/nxr/runs/`).
3. Hub inspector (or a CLI bridge) reads the preview JSON and links to chunked
   logs/events when event protocol v2 lands ([V4_EXECUTION_PROTOCOL.md](V4_EXECUTION_PROTOCOL.md)).
4. Cancel/rerun continue to use execution transport + supervisor paths already
   wired for `nxr session` remote attempts.

## Non-goals

- No NixPlane crate dependency in `nxr-core`.
- No Assignment → Realization storage in NXR.
- No requirement that `nxrd` be running for `nix run`.
