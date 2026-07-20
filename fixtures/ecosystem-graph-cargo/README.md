# Cargo workspace graph (read-only example)

This directory holds a **static, read-only** ecosystem graph snapshot. It
illustrates the adapter boundary documented in [docs/ADAPTERS.md](../../docs/ADAPTERS.md).

It is **not** executed by `nxr` and does **not** define operations. The
canonical leaf operations for this repository remain flake apps under
`apps.<system>.<name>`.

## Contents

| File | Purpose |
|---|---|
| [cargo-workspace-graph.json](cargo-workspace-graph.json) | Example `ecosystem-graph-v0` snapshot for a Cargo workspace |
| [README.md](README.md) | This note |

## Try the boundary (unit tests only)

```bash
cargo test -p nxr-core ecosystem::
```

The `nxr-core` crate embeds this JSON in adapter-boundary tests. No CLI command
consumes it in V2.3.

## What this demonstrates

- A `cargo-workspace` adapter id and repo-relative node ids.
- `suggested_apps` as **hints** that must still resolve to flake apps before execution.
- `confidence` labels on edges (`explicit` vs `inferred` vs `low`).
- Relationships (`depends_on`, `member_of`) that describe layout only.

## What this is not

- A mise, just, or Makefile importer.
- An alternate task manifest or project registry.
- A source of executable commands for `nxr run` or `nxr task`.
