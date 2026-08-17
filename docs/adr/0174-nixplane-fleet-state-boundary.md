# ADR-0174: NixPlane owns fleet desired state

- **Status:** Accepted
- **Date:** 2026-08-16
- **Target release:** n/a (ecosystem boundary)
- **Related ADRs:** ADR-0001, ADR-0101, ADR-0146
- **NixPlane ADRs:** 0003, 0005, 0012 (in the NixPlane repo)

## Context

The 2026-08-16 NixPlane ecosystem handoff names NXR as the operator DAG and
NixPlane as the fleet configuration/realization fabric. NXR already ships task
DAGs, execution contexts, and process-level secret delivery. Recreating those
inside NixPlane, or storing Profile Assignments inside NXR, would create two
sources of truth.

## Decision

1. **Keep the independent public `nxr` repository.** No merge into NixPlane. No
   git submodule. Consume via flake inputs and released packages.

2. **NXR never stores fleet desired state.** Profile Assignments
   (`Profile P → Realization R`) live on NixPlane Hub. NXR may orchestrate
   `compile → realize → assign → wait` as ordinary flake apps / tasks.

3. **Do not duplicate NXR's process-level secret system** in a NixPlane-only
   runner. A future optional provider may resolve NixPlane `BindingRef`s for one
   process; NXR remains usable without NixPlane.

4. **Do not block NixPlane Profile/NXD work on NXR V4.** A shared operation
   envelope can come later. WinExec events should map losslessly onto NXR
   events when that protocol exists.

5. **Do not import NixPlane fleet types** (PhysicalHost, Facet, Assignment) into
   NXR. A tiny shared BindingRef schema is the most that should ever cross.

## Consequences

- NixPlane pins a released NXR version (upgrade 3.4.0 → compatible 3.6.x).
- User-facing fabric workflows call `nixplane` leaves, not internal `nxb`/`nxd`
  binaries.
- ADR-0412 (prevent platform sprawl) remains in force: NXR is not a host
  activator or infrastructure state engine.

## Alternatives considered

- Merge NXR into NixPlane — rejected; independent audience and release cadence.
- NXR as the Assignment database — rejected; that is Hub/NixPlane.
- Wait for V4 operation IR before NixPlane NXD — rejected by NixPlane ADR-0003.
