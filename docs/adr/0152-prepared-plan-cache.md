# ADR-0152: Optional prepared app-plan disk cache

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 1a)
- **Related ADRs:** ADR-0010, ADR-0015, ADR-0134, ADR-0151

## Context

Warm invocations still rebuild command plans after process exit. Watch mode keeps
prepared plans in memory (`WatchCaches`), but `nxr plan` / `nxr run` re-run
discovery (or at least argv assembly) on every process. Wave 0 counters
(`NXR_PERF_STATS`) make `plan_prepare_us` visible; Wave 1a reduces that work when
inputs are unchanged.

ADR-0015 limited V1 caching to discovery metadata. Prepared plans are a separate,
optional layer: they store argv / plan envelopes, not evaluation results, and must
never persist secret values.

## Decision

1. Add an **optional** on-disk prepared-plan cache (`~/.cache/nxr/plans` or OS
   equivalent), schema version **1**, env kill-switch `NXR_PLAN_CACHE=off`
   (also `0` / `false` / `no`). Default: enabled when a cache directory exists.
2. Key entries by:
   - prepare kind (`fast` vs `discovered`)
   - flake identity (ref + local root)
   - system + app name (+ synthetic `apps.<system>.<name>` attr)
   - Nix executable path / version / file identity
   - optional Nix flags digest
   - shell name + shell-mode + active `NXR_DEV_SHELL` marker
   - working-directory policy (`--root` / `--cwd` + resolved paths)
   - environment **policy** digest (shape + CLI `--set` values; not secrets)
   - forwarded arguments
   - shared fingerprints: Nix-tree fingerprint, discovery-inputs fingerprint,
     optional `flake.lock` digest (ADR-0134 fingerprint machinery)
3. Miss → today’s prepare path, then store. Hit → reuse stored `Plan` + nix path +
   execution directory. **Live env and secrets are still resolved at spawn** —
   never from the cache entry.
4. Reject store/load when any `Plan.secrets[].value` is not the `<runtime>`
   placeholder (or empty). App prepare paths do not embed secret values today.
5. `nxr cache clear` / `status` include the prepared-plan cache. TTL backstop
   defaults to 24h (`NXR_PLAN_CACHE_TTL_SECS`; `0` disables).
6. Extend `NXR_PERF_STATS` to schema **v2** with `plan_cache_hits` /
   `plan_cache_misses` ([ADR-0151](0151-perf-counters.md)).
7. Preserve direct `nix run` as an escape hatch ([ADR-0010](README.md)); the cache
   is never required for correctness.
8. Expose `PlanCacheSharedFingerprints` so Wave 1b (store-executable reuse) can
   share fingerprint material without a second key design.

## Validation

- Unit tests: hit / miss / invalidation / secret rejection / clear / kill-switch.
- Warm `nxr plan` with `NXR_PERF_STATS=1` should show `plan_cache_hits ≥ 1` on a
  second identical invocation when fingerprints match.
- Disabled cache (`NXR_PLAN_CACHE=off`) matches pre-cache prepare behavior.

## Non-goals (Wave 1a)

- Store-executable direct exec (perf-1b).
- Daemon, Merkle trees, digest dedup, lazy scheduler.
- Caching task-node graphs or context secret bindings.
