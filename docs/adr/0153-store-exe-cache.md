# ADR-0153: Optional realised store-executable reuse

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** 3.2.0 / 3.2.1 (source-identity correctness in 3.2.1)
- **Related ADRs:** ADR-0010, ADR-0015, ADR-0147, ADR-0151, ADR-0152

## Context

Even when a prepared plan is reused (ADR-0152), every leaf still pays `nix run`
startup to re-enter the Nix CLI and re-resolve the app program. Once an app has
been realised into `/nix/store`, warm invocations can exec that program directly
when flake / Nix identity fingerprints are unchanged.

Workspace CAS must not absorb hermetic package store paths
([ADR-0147](0147-two-tier-actions.md)): this cache only records the realised
**program path** for spawn reuse, never as a workspace action artifact.

## Decision

1. Add an **optional** on-disk store-exe cache (`~/.cache/nxr/store-exe` or OS
   equivalent), schema version **2**, kill-switch `NXR_STORE_EXE_CACHE=off`
   (also `0` / `false` / `no`). Default: enabled when a cache directory exists.
2. Reuse `PlanCacheSharedFingerprints` from ADR-0152 for lock /
   discovery / Nix identity invalidation, plus **source identity**:
   - Git HEAD + porcelain scoped to the flake root (`git status --porcelain -- .`)
   - Declared `discoveryInputs` content fingerprint (hinted from discovery cache
     when a full discovery context is not yet available)
   - **Refuse** direct store-exe reuse when the tree is dirty and no
     `discoveryInputs` are known, or when neither git identity nor discovery
     inputs are available (non-git path flakes without declared inputs).
   Do **not** invent a second fingerprint scheme beyond these shared fields.
   Key material additionally includes flake ref, local root, system, app
   name / attr path, and Nix flags digest. Forwarded argv is **not** part of the
   key (args are applied at spawn). Key domain prefix is `store-exe-v2`.
3. **Miss / cold:** `nix eval --raw <flake>#apps.<system>.<app>.program`, then
   `nix build --no-link --print-out-paths <store-output>` when the program file
   is not yet usable; cache program + store output; spawn the program with the
   plan's forwarded arguments. This is intentional build-then-exec equivalence
   to `nix run` for ordinary flake apps.
4. **Hit:** verify the cached program path is still a usable executable → spawn
   it directly (0× `nix run`).
5. **Fallback:** disabled cache, develop/shell wrap, non-`nix run` plans,
   non-placeholder secret values, missing fingerprints / remote flakes, realise
   failure, or invalid store path → today's prepared `nix run` argv unchanged
   ([ADR-0010](README.md)).
6. Never key on secret values; reject eligibility when any
   `Plan.secrets[].value` is not the `<runtime>` placeholder.
7. `nxr cache clear` / `status` include the store-exe cache. TTL backstop
   defaults to 24h (`NXR_STORE_EXE_CACHE_TTL_SECS`; `0` disables).
8. Extend `NXR_PERF_STATS` to schema **v3** with `store_exe_hits` /
   `store_exe_misses`.
9. Wire into foreground app spawn, task leaf spawn, and process app spawn.
   Dry-run / `nxr plan` continue to show `nix run` (escape hatch visible).

### Interaction with prepared-plan cache

The two caches are independent:

- Plan cache hit → skips discovery / argv assembly; plan still describes `nix run`.
- Store-exe hit → may still occur on the same invocation and replace the spawn
  program with the store path while keeping forwarded args.
- Either miss falls back independently; disabling one does not disable the other.

## Validation

- Unit tests: hit / miss / invalidation / kill-switch / path parsing / eligibility.
- Integration: warm `nxr --flake fixtures/basic-apps hello` with `NXR_NIX`
  counting shim shows `run == 0` on the second invocation when fingerprints
  match; `NXR_STORE_EXE_CACHE=off` preserves classic `run == 1` budgets.
- Integration: package-backed `fixtures/package-app` — after a warm hit, editing
  `src/message.txt` must not spawn the previous `/nix/store` program (miss /
  re-realise / new greeting).

## Non-goals (Wave 1b)

- Daemon, Merkle trees, digest dedup, lazy scheduler, eval worker.
- Caching develop-wrapped or remote-flake apps.
- Treating store paths as workspace CAS outputs.
