# Plan: compilerCache spawn, cargo cohort, cargo-metadata affected

**Date:** 2026-09-03  
**Status:** direction for NXR agents  
**Demand:** `lattice-ecosystem/docs/dev/compiler-cache-plane.md`  
**Boundary:** [NIXPLANE.md](../NIXPLANE.md) — NXR does not store fleet
Assignments or run an object store. It **injects process env** and **selects
tasks**.

Lattice already copies `RUSTC_WRAPPER=sccache` and `SCCACHE_DIR` in every
cargo flake app. That is an NXR hole. Remote S3 (RustFS on a NixPlane Facet)
is configured **only** via env/Bindings; NXR must not hardcode Cloudflare.

Do **not** set `CARGO_INCREMENTAL=0` from `CI=true`. Lattice local
rust-validate depends on incremental rustc.

## 1. `perSystem.nxr.compilerCache.sccache`

**Where:** flake-parts module (`nix/`), spawn path next to execution
contexts ([TASK_SCHEMA_V2.md](../TASK_SCHEMA_V2.md),
[EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md)).  
**Schema:** additive on the v2 envelope or a top-level `compilerCache` map
(deny_unknown_fields — bump docs).

```nix
perSystem.nxr.compilerCache.sccache = {
  enable = true;
  # null → platform default; never put secrets here
  dir = null;
  # "inherit" | "off" | "on"
  incremental = "inherit";
  remote = {
    endpointEnv = "SCCACHE_ENDPOINT"; # empty env → L1 only
    bucketEnv = "SCCACHE_BUCKET";
    prefixFrom = "rustc-store-path";  # hash rustc sysroot into SCCACHE_S3_KEY_PREFIX
  };
};
```

Spawn behavior:

- Prepend `RUSTC_WRAPPER=sccache` when `enable` and `sccache` is on PATH.
- Set `SCCACHE_DIR` if unset.
- If `incremental = "inherit"`, do not touch `CARGO_INCREMENTAL`.
- If `endpointEnv` resolves, set sccache S3 vars; credentials come from the
  task **context** secrets (already shipped), not from this module.
- `prefixFrom = "rustc-store-path"`: at spawn, run `rustc --print sysroot`
  (or `which rustc`) and set `SCCACHE_S3_KEY_PREFIX` to
  `{host-triple}/{hash}`. Do not use umbrella nixpkgs rustc when the
  app’s flake is lattice.
- Plans/events: `compilerCache: sccache+remote|sccache|off` — never the
  secret, never the full key.

Tests: fixture flake with enable on/off; assert child env; assert CI=true
does not flip incremental.

Lattice follow-up (not this repo): delete the duplicated `export
RUSTC_WRAPPER` blocks in `lattice/flake.nix` after pinning an NXR release
that includes this.

## 2. Task `cohort` for exclusive language locks

**Where:** task-v2 field `cohort` (string).  
**Semantics:** tasks that share `(cohort, resources.exclusive lock name)`
and would have been scheduled as separate `-p` cargo leaves are **merged**
into one spawn. The merged argv is `cargo <verb> -p a -p b -p c` preserving
order. If merge cannot be proven equivalent (different apps, different
contexts, different params), do not merge; log a plan warning.

Fixture: three exclusive `cargo-target` leaves → one process. This is the
bug Lattice already patched by hand for sidecars; Stargate/winbuild will
copy it until NXR owns it.

## 3. `cargoPackageGraph` affected input

**Where:** affected-v2 / task `inputs`.  
**Today:** `inputs.paths = ["crates/**/*.rs"]` + Merkle index (ADR-0156)
still treats any crate rust file as “the whole rust-validate task.”  
**Change:** when a task declares

```nix
inputs.cargoPackageGraph = true; # or { manifest = "./Cargo.toml"; }
```

run `cargo metadata --format-version 1 --offline` (toolchain from the
task’s nix/shell). Build file → package → reverse-dep closure. A change
under `apps/desktop/src/**` that is not a rustc crate of `lattice-desktop`
must **not** select `rust-validate` unless metadata says so.

`--affected` remains conservative on metadata failure (run the task).
`--all` bypasses.

Do not wait on deferred ADR-0205’s full git+nix soup. This is a **cargo
adapter** behind the existing affected pipeline.

Acceptance for Lattice: dirty `apps/desktop/src/help/*.ts` only →
`nxr task --affected` does not start `rust-validate`. Dirty
`crates/lattice-core/src/*.rs` → does.

## 4. Docs: exclusive lock is per machine

In [TASK_SCHEMA_V2.md](../TASK_SCHEMA_V2.md) `resources` and CI plan docs:
two VMs with the same exclusive lock compile twice. Combining clippy+test
on one VM (`rust-validate`) is the supported identity. Schema should
**warn** authors who split those as two GHA matrix leaves.

## 5. Optional disk leaf

A stock app or documented recipe `cargo-gc` that removes `target/` older
than N days / worktree leftovers. Not required for compilerCache.

## Non-goals

- Implementing RustFS or Tailscale (NixPlane NXD)
- Storing Profile Assignments (NixPlane Hub)
- Remote workspace CAS for `target/` ([#2](https://github.com/willmortimer/nxr/issues/2) is not this)
- Inferring incremental-off from `CI`

## Release

Ship as NXR **3.7.x** (or whatever is current after 3.6.0). Do not tell
Lattice to track `main`. Pin a tag; lattice-ecosystem and lattice bump
`inputs.nxr`.
