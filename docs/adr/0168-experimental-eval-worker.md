# ADR-0168: Experimental optional Nix eval worker via `nxrd`

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 8c)
- **Related ADRs:** ADR-0157, ADR-0164, ADR-0165, ADR-0166

## Context

Repeated metadata evaluation, completion, listing, and graph inspection pay
Nix CLI process startup on every cold `nix eval --json`. Discovery and
prepared-plan caches already amortize *parsed* workspace results across
invocations ([ADR-0157](0157-optional-nxrd.md)), but narrow attr evals that
miss those caches still respawn `nix`. ADR-0157 reserved `eval.prepare`;
ADR-0165 reserved `eval_worker_eligible` for Determinate-oriented hosts.

A durable in-process Nix evaluator (libnix / Determinate eval server) is not
yet a stable, portable dependency for nxr. This wave ships an **experimental,
opt-in** worker façade that keeps list/metadata-shaped JSON warm inside
`nxrd` and always falls back to subprocess `nix eval`.

## Decision

1. **Opt-in only:** `NXR_EVAL_WORKER=1` (also `true` / `on` / `yes`). Default
   **off** — absent or any other value leaves today's subprocess path unchanged.
2. **Eligibility:** clients consult
   [`DiscoveryEvalPlan::eval_worker_eligible`](../../crates/nxr-nix/src/strategy.rs)
   (Determinate distribution today). Non-eligible hosts never attempt the
   worker even when the env opt-in is set.
3. **Transport:** reuse the ADR-0157 Unix-socket JSON-lines protocol. Implement:
   - `eval.prepare` — bind a session to `nix_identity`, optional
     `config_fingerprint`, `flake_root`, and `flake_fingerprint`; clear cache
     entries when identity/config change or the flake fingerprint for that
     root changes.
   - `eval.get` / `eval.put` — retain narrowly typed JSON for kinds
     `metadata` | `tasks` | `list` only (attr/list-shaped payloads).
   - `worker.register` remains `not_implemented` (no remote worker registry).
4. **Authority:** the worker is **not** flake-evaluation authority and **not**
   an execution scheduler. Clients run `nix eval` on miss / doubt / absent
   daemon / protocol mismatch / non-eligible host, then may `eval.put` the
   stdout JSON. Cached JSON is never a trust boundary for spawn.
5. **Invalidation:** any change to nix identity, config fingerprint, or
   per-root flake fingerprint (client-supplied; typically flake.nix /
   flake.lock / discovery-input digests) drops affected entries. Secrets must
   never appear in worker logs or protocol fields.
6. **Non-goals (this wave):**
   - Reimplementing flake evaluation or embedding libnix.
   - Making the worker required for correctness or CI.
   - General-purpose `--expr` eval, builds, or `nix run` mediation.
   - Cross-machine / remote workers.

## Validation

- Unit tests: opt-in parser default-off; absent-worker / disabled path is a
  no-op; prepare invalidation; get/put round-trip on a temp daemon socket.
- Integration must not hang CI: no long-lived Nix subprocess in the default
  test path; optional nix-backed checks remain skippable via existing
  `NXR_SKIP_NIX_INTEGRATION`.

## Program complete notes (perf-8c)

- Experimental opt-in path landed: daemon methods + client helper wired into
  `nxrMetadata` / tasks eval helpers when enabled and eligible.
- Default path unchanged when `NXR_EVAL_WORKER` is unset.
- Docs warn experimental; true warm Nix evaluator remains a follow-up if
  Determinate (or another) exposes a stable long-lived eval API.

## Consequences

- Operators may set `NXR_EVAL_WORKER=1` with `nxr daemon start` to reuse
  metadata/list JSON across invocations on Determinate hosts.
- Subprocess `nix eval` remains the correctness path.
- Documented in [PERFORMANCE.md](../PERFORMANCE.md) and CHANGELOG as
  experimental / not required.
