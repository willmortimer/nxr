# Plan: nextest archive pilot (fixtures/nextest-pilot)

**Date:** 2026-11-16  
**Status:** measurement pending  
**Worktree pilot:** `fixtures/nextest-pilot` (`measure.sh` writes notes for the parent)

## Scope

This pilot explores **cargo-nextest test archives** on a standalone tiny crate.
It is **optional** instrumentation for comparison notes only. It does **not**
replace the flake `apps.test` leaf or change how `nxr task test` runs
(`cargo nextest run --workspace` remains the canonical workspace test app).

## Procedure (parent-owned)

From `fixtures/nextest-pilot`, run `./measure.sh <notes-path>`. The script
times `cargo test --doc` and `cargo test --lib`, then—when `cargo nextest` is on
`PATH`—creates `pilot.tar.zst` under `CARGO_TARGET_DIR` (or `target/`) and runs a
filtered nextest execution from that archive. It never deletes caches or invokes
`nix-collect-garbage`.

## Measurement table

Values below are placeholders until the parent run fills `measure.sh` output.

| Metric | Value | Notes |
| --- | --- | --- |
| compile time | not_measured | |
| archive create time | not_measured | |
| archive bytes | not_measured | |
| filtered execution time | not_measured | |
| warm execution time | not_measured | |
| doctest coverage | doctests are outside the nextest archive unless `cargo test --doc` is run separately | limitation |
| custom harnesses | not covered by the default nextest archive | limitation |
| portability | an archive is not portable across operating systems or incompatible rustc | limitation |

## Recommendation

Retain nextest archives as an **optional** local optimization and documentation
aid until measured numbers from `measure.sh` are copied into the table above.
No change to `apps.test` or workspace CI gates is proposed on pilot evidence
alone.