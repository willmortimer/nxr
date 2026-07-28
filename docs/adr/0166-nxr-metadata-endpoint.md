# ADR-0166: Optional `nxrMetadata` single-evaluation discovery endpoint

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 9)
- **Related ADRs:** ADR-0150, ADR-0015

## Context

Cold discovery still pays either:

- `nix flake show --json` plus a separate `nix eval` of `nxr.<system>`, or
- one Determinate-oriented coalesced `--expr` ([ADR-0150](0150-inventory-coalesce.md))
  that reconstructs inventory + tasks via `builtins.getFlake`.

Ideal flake-parts consumers already know their apps, tasks, processes, contexts,
and inventory names at evaluation time. A compact, JSON-serializable flake
output can collapse discovery to **one targeted attr eval** without requiring
Determinate parallel eval or replacing standard outputs.

## Decision

1. The flake-parts module optionally emits
   `nxrMetadata.<system>` (default **on** for module consumers;
   `nxr.metadata.enable = false` disables emission).
2. Document shape (envelope `schema_version = 1`):
   - `task_schema_version` — major version for embedded task fields
   - `apps` — listing metadata (description/category); non-authoritative
   - `tasks` / `processes` / `contexts` / `discoveryInputs` — same JSON as
     `nxr.<system>` subsets
   - `inventory` — name lists for `apps` / `packages` / `checks` / `devShells`
   - `namespaces` — optional non-authoritative views (`nxr.namespaces`, same
     membership shape as `nxr.projects.json`)
3. Rust cold discovery preference order:
   1. `nxrMetadata.<system>` when preference is enabled (default; kill-switch
      `NXR_NXR_METADATA=off`)
   2. Coalesced `{ inventory, nxr }` when available
   3. Classic `flake show` + task `eval`
4. Missing `nxrMetadata` is **not** an error — silent fallback. Other eval
   failures warn and fall back.
5. Standard flake outputs and `nxr.<system>` remain authoritative for
   execution and schema validation. `nxrMetadata` is never required.

## Validation

- Fixture `fixtures/nxr-metadata` evaluates `nxrMetadata.<system>` and discovers
  apps/tasks/namespaces in one `nix eval --json`.
- Unit/integration tests cover parse, missing-attr fallback, and single-eval
  cold discovery budget when the output is present.
- Flakes without the output keep today's coalesce/show+eval path
  (`NXR_NXR_METADATA=off` isolates coalesce-only budgets in tests).

## Consequences

- Flake-parts consumers get a cheaper cold discovery path on all Nix
  distributions (not only Determinate).
- Flakes that do not emit `nxrMetadata` pay one failed attr eval before
  fallback unless `NXR_NXR_METADATA=off`.
- Envelope versioning is independent of task document majors
  (`schema_version` vs `task_schema_version`).
