# ADR-0144: Auto-promote task documents to schema v2 for security fields

- **Status:** Accepted
- **Date:** 2026-07-27
- **Target release:** 2.7.1
- **Related ADRs:** ADR-0120, ADR-0122

## Context

Emitting `contexts`, `task.context`, `task.shell`, `inputs`, `outputs`, `cache`,
or `resources` inside `schema_version: 1` lets older runners silently ignore
execution/security metadata while still running the app — the failure mode
schema v2 exists to prevent.

## Decision

The flake-parts module **must** emit `schema_version = 2` whenever any
v2-only or security/execution field is present (top-level `contexts`, or any
task field among `context`, `shell`, `inputs`, `outputs`, `cache`, `resources`).
Optional `perSystem.nxr.schemaVersion` may force `2` explicitly; it must not
force `1` when those fields are present (hard evaluation error).

Older runners reject unsupported majors instead of degrading silently.

## Validation

- Fixture with contexts evaluates to `schema_version: 2`.
- Pure v1 tasks still emit `schema_version: 1`.
- Attempting `schemaVersion = 1` with contexts fails evaluation.
