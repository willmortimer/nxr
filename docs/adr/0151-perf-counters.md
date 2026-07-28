# ADR-0151: Optional perf counters via `NXR_PERF_STATS`

- **Status:** Accepted
- **Date:** 2026-07-28
- **Target release:** Unreleased (perf Wave 0)
- **Related ADRs:** ADR-0011, ADR-0017

## Context

Performance work on nxr needs measurement-driven baselines beyond black-box wall
times. Subprocess counts, hashing volume, plan-prepare latency, CAS lookup time,
and spawn-to-first-child-output are not visible from `measure-release.sh` alone.

## Decision

1. Add an **optional**, env-gated counter facility in `nxr-core` (`NXR_PERF_STATS=1`).
2. When disabled (default), counters are not accumulated and process behavior is
   unchanged.
3. When enabled, counters are emitted as a single JSON line on stderr at process
   exit (`nxr-perf-stats: {…}`), schema version **3** (v1/v2 fields retained;
   `store_exe_hits` / `store_exe_misses` added in ADR-0153; plan-cache counters
   in ADR-0152).
4. Do **not** add a public CLI flag in Wave 0; env gate avoids CLI contract churn.
5. Extend `scripts/perf/measure-matrix.sh` for additional scenarios; document
   deferred/flaky cases in `docs/PERFORMANCE.md`.

### Counter surface (schema v3)

| Field | Meaning |
|---|---|
| `nix_spawns` | `run_nix` and supervised `nix` child spawns |
| `fs_metadata` | `metadata` / stat probes on discovery fingerprint paths |
| `bytes_hashed` | Bytes fed through BLAKE3 on hot paths |
| `plan_prepare_us` | Accumulated plan-prepare wall time (µs) |
| `cas_lookup_us` | Accumulated workspace CAS lookup wall time (µs) |
| `spawn_to_child_output_us` | First child stderr byte after spawn (µs), when piped |
| `plan_cache_hits` | Prepared-plan disk cache hits |
| `plan_cache_misses` | Prepared-plan disk cache misses (prepare ran) |
| `store_exe_hits` | Store-exe disk cache hits (direct store spawn) |
| `store_exe_misses` | Store-exe disk cache misses |

## Validation

- Unit tests in `nxr-core::perf` assert accumulation when enabled and no-op when off.
- `measure-release.sh` thresholds unchanged; matrix script is additive.
- Wave 1 may consume these counters for prepared-plan cache and store-exe reuse.

## Non-goals (Wave 0)

- Prepared-plan cache, daemon, Merkle, or persistent perf telemetry export.
