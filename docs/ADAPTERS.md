# Ecosystem graph adapters

## 1. Purpose

Ecosystem graph adapters are **optional, read-only** readers for adjacent
project metadata (Cargo workspaces, Node workspaces, Terraform modules, and
similar). They help `nxr` **describe** relationships between packages and
projects inside a flake-backed repository.

Adapters exist to support future monorepo ergonomics — navigation, affected
analysis, and suggestions — without introducing a second operation authority.

## 2. Non-authority rule

The following remain fixed by [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) and
[ARCHITECTURE.md](ARCHITECTURE.md):

| Layer | Role | Authority |
|---|---|---|
| Flake apps (`apps.<system>.<name>`) | Canonical leaf operations | **Executable** |
| V2 tasks (`nxr.<system>`) | Orchestration over apps | Coordinates apps; does not replace them |
| Ecosystem graph adapters | Relationship descriptions | **Read-only**; never execute |

Adapters may:

- list projects or packages and how they relate;
- attach **confidence** labels to inferred edges;
- suggest flake **app names** that might exist for a node.

Adapters must **not**:

- define runnable commands or shell snippets;
- install language runtimes or packages;
- override flake apps, tasks, or Nix evaluation;
- become the only way to run work in a repository;
- silently drive destructive workflows from low-confidence inference.

If an adapter output disagrees with the flake, **the flake wins**.

Direct `nix run` and `nxr <app>` remain supported escape hatches.

## 3. Relationship to other “adapters”

`nxr` uses the word *adapter* in two distinct places:

| Name | Crate / doc | Purpose |
|---|---|---|
| **Nix adapter** | `nxr-nix` ([ARCHITECTURE.md](ARCHITECTURE.md) §4.3) | Locate `nix`, negotiate CLI capabilities, construct argv for `nix run` |
| **Ecosystem graph adapter** | `nxr-core::ecosystem` (this doc) | Read-only project relationship snapshots |

The Nix adapter is on the **execution path**. Ecosystem graph adapters are
**not** on the execution path in V2.3.

## 4. Boundary interface (Rust)

The exploratory Rust surface lives in `nxr-core`:

```rust
pub trait EcosystemGraphAdapter {
    fn adapter_id(&self) -> &str;
    fn read_graph(&self, workspace_root: &str) -> Result<EcosystemGraph, AdapterError>;
}
```

`EcosystemGraph` is a versioned snapshot (`schema_version: 0` today) with:

- `nodes` — stable ids, display labels, optional `suggested_apps` hints;
- `edges` — directed relationships with `kind` and `confidence`.

`StaticJsonAdapter` loads fixed JSON for documentation and unit tests. Future
implementations (Cargo, Node, and so on) would implement the same trait but are
**not** product features in V2.3.

## 5. Machine-readable shape

Exploratory JSON schema: [schemas/ecosystem-graph-v0.schema.json](../schemas/ecosystem-graph-v0.schema.json).

Read-only example fixture:
[fixtures/ecosystem-graph-cargo/](../fixtures/ecosystem-graph-cargo/).

```json
{
  "schema_version": 0,
  "adapter_id": "cargo-workspace",
  "workspace_root": ".",
  "nodes": [
    {
      "id": "crates/nxr-cli",
      "label": "nxr-cli",
      "kind": "package",
      "suggested_apps": ["test"]
    }
  ],
  "edges": [
    {
      "from": "crates/nxr-cli",
      "to": "crates/nxr-core",
      "kind": "depends_on",
      "confidence": "explicit"
    }
  ]
}
```

`suggested_apps` are **not** resolved to `apps.<system>.<name>` by adapters.
Resolution and execution stay in flake discovery and the Nix adapter.

## 6. Confidence and overrides

| Confidence | Meaning | Policy |
|---|---|---|
| `explicit` | Declared in project metadata | Safe for display and high-trust planning hints |
| `inferred` | Derived from layout or tooling output | Show in inspect/graph UX; require confirmation before side effects |
| `low` | Weak or heuristic signal | Must not silently affect destructive workflows |

Explicit flake metadata (`nxr.<system>` tasks, categories, aliases) always
overrides adapter inference when both are present.

## 7. Explicit non-goals (V2.3)

The following are **out of scope** for this milestone:

- mise, just, Make, or Taskfile **importers** as shipped product features;
- mandatory project manifests or a second task language;
- adapter-driven execution (`nxr run` / `nxr task` must not call adapters);
- CI provider adapters or remote execution hooks;
- daemon state or a persistent project database.

Deferred design for full monorepo intelligence lives in
[ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md) (Phase 18+).

## 8. Testing the boundary

Unit tests in `nxr-core` parse the Cargo fixture and exercise
`EcosystemGraphAdapter` without Nix:

```bash
cargo test -p nxr-core ecosystem::
```

No CLI subcommand consumes adapter output in V2.3. CLI integration belongs to
later milestones after the non-authority contract is stable.

## 9. Future wiring (not implemented)

When adapters are wired into inspect or graph surfaces, the expected flow is:

```text
flake discovery (apps, tasks)     ← authoritative for execution
        +
ecosystem adapter (optional)      ← descriptive overlay only
        ↓
inspect / graph / affected UX     ← human or machine presentation
```

Execution plans ([`plan`](CLI_CONTRACT.md), task scheduler) must continue to
reference flake apps and evaluated task metadata only.

## 10. See also

- [ARCHITECTURE.md](ARCHITECTURE.md) — layer model and Nix adapter
- [COMPATIBILITY.md](COMPATIBILITY.md) — V2.x extension points
- [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md) — product synthesis rules
- [ROADMAP.md](ROADMAP.md) — 2.3 monorepo ergonomics deliverable
- [MIGRATE_FROM_MISE_JUST.md](MIGRATE_FROM_MISE_JUST.md) — flake-first migration (not adapter import)
