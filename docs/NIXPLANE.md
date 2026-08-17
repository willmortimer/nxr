# NixPlane boundary

NixPlane is a **sibling product**, not an NXR subsystem.

Canonical fabric design lives in the NixPlane repo:
`docs/ecosystem/README.md` (snapshot absorbed 2026-08-16). NXR-specific notes:
that package's `docs/ecosystem/nxr.md`.

## Identity

NXR remains an independent public project:

> A Nix-native command, workflow, process, and execution-context plane over standard flake outputs.

It is not NixPlane's fleet database, deployment controller, system activator, or secret store.

## Ownership

| Layer | Owns |
| --- | --- |
| **NXR** | What command/task runs, DAG edges, process execution context, process-level secret delivery, stdout/stderr/events, local/CI equivalence |
| **NixPlane** | PhysicalHosts/Facets, Profile desired state (Assignment → Realization), environments, builder/cache topology, deployment health, reconciliation/rollback |
| **NXB** | Nix realization / publication (crate inside NixPlane) |
| **NXD** | Profile activation (crate inside NixPlane) |
| **Hub** | Desired/observed/history |

A NixPlane workflow can stay an ordinary NXR graph:

```text
nxr task deploy-staging
  → tests
  → nixplane config compile
  → nixplane realize
  → nixplane assign staging
  → nixplane wait --healthy
```

Standard flake apps remain valid leaf operations. NXR must never store the
target Profile's desired Assignment.

## Secrets

NXR already has process-level secret refs and providers. Do not duplicate that
inside NixPlane, and do not import full NixPlane fleet types into NXR.

Long-term optional provider: NXR asks a local NixPlane Binding service for a
`BindingRef`. Without NixPlane, NXR keeps working with its current providers.

## Windows

Current: NXR DAG → WinBuild.

Future: NXR DAG → WinExec operation → native Windows host. NXR owns dependency
edges; WinExec owns process transport.

## Sequencing

Do not block NixPlane Profile/NXD work on NXR V4. NixPlane should upgrade its
3.4.0 pin to compatible 3.6.x. Consume via flake input / released packages; no
git submodule.

See [ADR-0174](adr/0174-nixplane-fleet-state-boundary.md).
