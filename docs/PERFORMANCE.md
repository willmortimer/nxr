# Performance (V1 / V2)

Baselines for the runner. App **execution** time is dominated by `nix run` and the app itself; `nxr` overhead is discovery, planning, and process supervision.

## North star

When discovery and capability caches are warm and no Nix re-evaluation is required,
**nxr overhead before the child starts** (plan/prepare through `nix` spawn) should
stay in the **tens of milliseconds** on a local SSD — not hundreds. Use
`NXR_PERF_STATS=1` and `plan_prepare_us` plus black-box `measure-matrix.sh` warm
dry-run scenarios to track this; child execution time is excluded.

## Instrumentation (`NXR_PERF_STATS`)

Set `NXR_PERF_STATS=1` to accumulate counters for one CLI invocation. On exit,
nxr prints a single JSON line on stderr:

```text
nxr-perf-stats: {"schema_version":5,"nix_spawns":…,…}
```

| Counter | Meaning |
|---|---|
| `nix_spawns` | `run_nix` and supervised `nix` child spawns |
| `fs_metadata` | Filesystem metadata probes on fingerprint paths |
| `bytes_hashed` | Bytes hashed (BLAKE3) on hot paths |
| `plan_prepare_us` | Plan/prepare wall time (µs, accumulated) |
| `cas_lookup_us` | Workspace CAS lookup wall time (µs) |
| `spawn_to_child_output_us` | Spawn to first piped child stderr byte (µs) |
| `plan_cache_hits` | Prepared-plan disk cache hits |
| `plan_cache_misses` | Prepared-plan disk cache misses |
| `store_exe_hits` | Store-exe disk cache hits (direct `/nix/store` spawn) |
| `store_exe_misses` | Store-exe disk cache misses (realise or `nix run` fallback) |
| `digest_cache_hits` | Run-scoped path/pattern digest cache hits (action-key planning) |
| `digest_metadata_hits` | Action-digest reuse when device/inode/size/mtime(/ctime) match |
| `git_blob_digests` | Action digests derived from Git blob OID (no working-tree read) |

Counters are **off by default**; no semantic change when unset. See
[ADR-0151](adr/0151-perf-counters.md), [ADR-0152](adr/0152-prepared-plan-cache.md),
[ADR-0153](adr/0153-store-exe-cache.md), [ADR-0154](adr/0154-run-digest-cache.md),
and [ADR-0155](adr/0155-incremental-git-digests.md),
and [ADR-0156](adr/0156-merkle-affected-index.md),
and [ADR-0157](adr/0157-optional-nxrd.md).

## Optional local cache daemon (`nxrd`)

Optional per-user daemon for warm multi-invocation sessions
([ADR-0157](adr/0157-optional-nxrd.md)):

```bash
nxr daemon start          # background
nxr daemon status --json
nxr daemon stop
```

- Socket: `$XDG_RUNTIME_DIR/nxr/nxrd.sock` (override `NXR_DAEMON_SOCKET`).
- Protocol: JSON lines, version **1**, role `cache` only — not execution
  authority; reserved methods leave room for lazy prep (4b), log broker (7c),
  and workers without claiming them.
- **Retained in RAM while running:** discovery payloads, prepared plans
  (placeholder secret policy), fingerprint strings, Merkle invalidation path
  sets, recent action-key digests.
- **Not retained / not authoritative:** secret values, Nix eval results as a
  trust boundary, process supervision, remote workers.
- Kill-switch: `NXR_DAEMON=off` (also `0` / `false` / `no`). Absent socket or
  protocol mismatch → identical standalone CLI behavior.
- Watch best-effort calls `merkle.invalidate` on restart classification; full
  `MerkleSession` ownership in-daemon is Wave 5.

## Run-scoped digest cache

Per-invocation memo for workspace action-key hashing ([ADR-0154](adr/0154-run-digest-cache.md)).
Overlapping `inputs.paths` / discovery inputs across task nodes share digest results
within one `nxr task` / plan pass.

## Incremental action digests + Git blobs

Warm and large-repo path hashing for action keys ([ADR-0155](adr/0155-incremental-git-digests.md)):

- **Metadata gate:** durable per-root index (`…/nxr/action-digests/`) reuses a prior
  content digest when device/inode/size/nanosecond mtime (and ctime on Unix) match —
  same pattern as discovery fingerprint indexes, but a **separate** store.
- **Git clean tracked:** digest =
  `BLAKE3("nxr.action-digest.git-blob.v1" ‖ NUL ‖ oid_hex)` from a batched
  `git ls-files --stage` + `git status --porcelain -z` (no per-file `git`).
- **Dirty / untracked:** BLAKE3 of working-tree bytes.
- Kill-switches: `NXR_GIT_DIGESTS=off`, `NXR_ACTION_DIGEST_INDEX=off`.
- `cas::digest_repo_path` (CAS verify/save) stays pure content hashing.
- Wave 3 Merkle leaves should reuse these per-file digests; directory digests still
  walk children today.

## Repository Merkle / directory index

Directory digests for action keys ([ADR-0156](adr/0156-merkle-affected-index.md)):

- Immediate-child aggregation (`nxr.merkle.dir.v1`) over Wave 2b leaf digests so a
  directory digest changes only when a descendant changes.
- Durable index `…/nxr/merkle-index/` (schema **v1**), separate from discovery and
  action-digest indexes. Kill-switch: `NXR_MERKLE_INDEX=off` (flat walk; matches
  pre-Wave-3 directory digests).
- **One-time action-key churn** when Merkle is on: directory-shaped `inputs.paths`
  digests differ from the flat formula; file-only inputs are unaffected by this
  change. `nxr cache clear` / `status` cover the merkle index.
- Affected analysis skips ownership checks for nodes whose path-root prefix cannot
  overlap a change (sibling locality).
- After `invalidate_paths` in a long-lived session, unrelated directory digests
  stay memoized (edit locality). Cold CLI rebuilds from the filesystem.

Bounded large-tree locality is covered by
`cargo test -p nxr-core --lib merkle_index::tests::large_tree_dir_digest_is_stable_and_local`
(~200 files). Wire into `measure-matrix.sh` later if black-box wall time is needed.

## Prepared-plan disk cache

Optional cache of prepared app command plans (argv / plan envelope) keyed by flake
identity, system, app, Nix identity + flags, shell/cwd/env **policy** digests, and
discovery/lock fingerprints. Schema **v1**. Miss → today’s prepare path; hit →
reuse the stored plan. Live environment and secret values are still resolved at
spawn — never stored. Set `NXR_PLAN_CACHE=off` to disable. TTL default 24h
(`NXR_PLAN_CACHE_TTL_SECS`; `0` disables). `nxr cache clear` / `status` cover this
cache alongside discovery, capabilities, workspace CAS, and store-exe. See
[ADR-0152](adr/0152-prepared-plan-cache.md).

## Store-exe disk cache

Optional cache of realised flake-app store executables. Schema **v1**. Miss/cold →
`nix eval` of `apps.<system>.<app>.program` plus `nix build --no-link
--print-out-paths` when needed, then direct exec (build-then-exec equivalence to
`nix run`). Hit → spawn the cached `/nix/store/…` program with forwarded args
(0× `nix run`) when fingerprints match and the path is still valid. Falls back to
`nix run` for shell wraps, doubt, or `NXR_STORE_EXE_CACHE=off`. Reuses
`PlanCacheSharedFingerprints` from ADR-0152. Independent of the prepared-plan
cache (both may hit on one invocation). TTL default 24h
(`NXR_STORE_EXE_CACHE_TTL_SECS`; `0` disables). See
[ADR-0153](adr/0153-store-exe-cache.md).

## Process supervision

Parallel and multiplexed `nxr task` runs pipe every supervised child's stdout/stderr through a single `mio` poll loop (kqueue on macOS, epoll on Linux) in `nxr-process`. FDs are `O_NONBLOCK`; each readiness event drains until `WouldBlock`/EOF within a per-FD fairness budget (1 MiB). Pipe registrations outlive process exit until EOF so rapid-exit output is not discarded ([ADR-0143](adr/0143-mio-pipe-drain.md)). One reusable 32 KiB read buffer is shared across registered fds; compact `u32` node ids map back to task labels when emitting events. Per-node timeouts use a min-deadline heap with O(log n) nearest-deadline lookup (lazy cancelled-entry prune).

Windows builds still use one reader thread per pipe (Unix-first); the supervisor API is unchanged.

## Nix call budgets

| Path | Expected Nix invocations | Notes |
|---|---|---|
| Bare `nxr <app>` / `nxr run <app>` with `NXR_STORE_EXE_CACHE=off` | **exactly 1×** `nix` (`nix run`); **0×** probes / `flake show` | Classic budget; locate-only prepare |
| Bare app warm with store-exe cache hit | **0×** `nix run` | Direct `/nix/store` spawn; cold may `eval`/`build` then exec |
| Bare app missing installable (non-TTY stderr, store-exe off) | **1×** `nix run` + optional diagnostic discovery | Bounded stderr tail (~128 KiB); suggestion discovery only when stderr indicates installable-resolution failure |
| Bare app on a TTY (store-exe off) | **1×** `nix run`; inherit stderr | Prefer transparent rendering over typo suggestions |
| Adapter init (list/task/doctor) | **1×** `nix eval` (`currentSystem`) + capability probes (`--version`, config/help) | Shared via `WorkspaceSnapshot` / `NixAdapter`; warm capability cache skips all probes when the environment digest matches |
| `nxr task` with **N** nodes (store-exe off) | **N×** `nix run` + **O(1)** discovery | One `flake show` (apps) + one task `eval` (or warm combined cache); **not** N× `flake show` |
| `nxr list --refresh-discovery` | Dominated by `nix flake show` | Catalog commands still discover |
| Named `nxr build` / `check` / `shell` | Direct installable argv (no whole-output discovery up front) | Adapter init still probes once for system / flags |
| Named build/check/shell missing attribute | **1×** installable + optional diagnostic discovery | Suggestion discovery only when stderr indicates missing attribute |

Instrumented integration tests wrap `NXR_NIX` with a counting shim to assert these budgets (`run==1`, `eval==0`, `flake-show==0`, `other==0` for bare success/fail). Capability probes are logged separately as `version`, `config`, and `help` (not lumped into `other`).

## Budgets

| Path | Budget | Notes |
|---|---|---|
| Interactive completion (`nxr __complete apps`) | ≤ **500 ms** cold discovery wait | [`DISCOVERY_TIMEOUT`](../crates/nxr-completion/src/dynamic.rs); empty candidates on timeout |
| Warm `nxr list` (cache hit) | Interactive (tens of ms) | Combined apps+tasks discovery cache |
| Cold `nxr list --refresh-discovery` | Dominated by `nix flake show` + one task eval | Nix eval/store caches still apply |

Discovery cache **schema v5** (incremental fingerprint index) invalidates on:

- **Content-correct hashes** of every `*.nix` file under the local flake root
  (path-scoped walk), plus `flake.lock` when present — not arbitrary non-Nix
  sources. Unchanged files **reuse** a prior BLAKE3 when **device/inode/size/
  nanosecond mtime** (and **ctime on Unix**) match (metadata-gated reuse, not a
  full re-read each warm hit). The on-disk index is compact JSON and is not
  rewritten when the computed index is unchanged. Legacy pretty-printed v1
  indexes still load. Tools that rewrite bytes while preserving those metadata
  fields can theoretically evade rehash until
  `NXR_FINGERPRINT_FORCE_REHASH_SECS` / TTL / `--refresh-discovery`; that is
  uncommon. **Git fsmonitor / watchman** integration is a future optimization
  (out of scope for V1).
- **Nix identity**: canonical Nix executable path + version string
- **Discovery schema version** (`nxr.<system>` / task document major)
- **Sorted `discoveryInputs`**: content-correct hashes of paths declared via
  `perSystem.nxr.discoveryInputs` (same metadata-gated incremental index as the
  Nix tree; hashed on store/load without a second eval on warm hits)
- **TTL backstop**: default 24h (`NXR_CACHE_TTL_SECS`; `0` disables)

Built-in ignores cover `.git`, `result`, `target`, and similar trees. Set `NXR_CACHE_FINGERPRINT_IGNORE` to a colon-separated list of globs to skip huge vendored `.nix` trees. Remote flakes are never cached.

Capability cache (schema **v4** as of 2.7.1) invalidates on Nix executable identity (canonical path + device/inode + size + mtime) **and** an environment digest that includes `NIX_CONFIG` / path lists **plus config-file identity** (size/mtime/ctime/content hash for known conf files — [ADR-0145](adr/0145-capability-config-files.md)), with `nix config show --json` as a store-time / miss backstop and a 7-day TTL (`NXR_CAPABILITY_CACHE_TTL_SECS`; `0` disables). Warm hits with a matching environment digest skip all capability probes. When the binary layer is warm but the environment digest changed, only config is re-probed. Set `NXR_CAPABILITY_CACHE=off` to bypass. `nxr cache clear` removes capability entries alongside discovery cache.

## Measured baselines

### Release (`nix build .#nxr`) — 2026-07-26

Host: `aarch64-darwin`, macOS 26.5.1, Determinate Nix 3.21.8 / Nix 2.34.8.
Harness: [`scripts/perf/measure-release.sh`](../scripts/perf/measure-release.sh) (`NXR_PERF_RUNS=5`).
Artifact: [`scripts/perf/baseline-aarch64-darwin.json`](../scripts/perf/baseline-aarch64-darwin.json).

| Scenario | p50 wall time | Observations |
|---|---|---|
| Cold `list` (`fixtures/basic-apps`, `--refresh-discovery`) | **0.16 s** | First cold sample ~0.57 s; later samples benefit from Nix eval cache |
| Warm `list` (`fixtures/basic-apps`) | **0.01 s** | Discovery + capability cache; under 25 ms target |
| Warm `plan hello` (`fixtures/basic-apps`) | **≤ 0.01 s** | Resolve + plan only |
| Warm `list` (this repo) | **0.01 s** | Three-run spot check after `--refresh-discovery` |

Warm list is ~5× faster than the prior **debug** baseline (~0.05 s) on the same host class. Capability-cache integration tests assert warm `list` Nix call budgets (`version=0`, `help=0`, `config=0`, `flake-show=0`). CI enforces warm/cold p50 ceilings via [`scripts/perf/measure-release.sh --enforce`](../scripts/perf/measure-release.sh) and [`scripts/perf/ci-thresholds.json`](../scripts/perf/ci-thresholds.json).

### Debug (historical, 2026-07-18)

Host: `aarch64-darwin`, macOS 26.5.1, Nix 2.34.7. Binary: `cargo build -p nxr-cli` (debug).

| Scenario | Avg wall time | Observations |
|---|---|---|
| Cold `nxr --refresh-discovery list` (this repo) | **0.62 s** | First refresh ~1.5 s; later refreshes ~0.17 s (Nix evaluation cache) |
| Warm `nxr list` (this repo) | **0.05 s** | Discovery cache hit |
| Cold `nxr --flake ./fixtures/basic-apps --refresh-discovery list` | **0.18 s** | Small fixture flake |
| Warm `nxr --flake ./fixtures/basic-apps list` | **0.05 s** | Cache hit |
| Warm `nxr __complete apps` | **0.05 s** | Within completion budget |
| `nxr plan test` | **0.17 s** | Resolve + plan; no app execution |

Re-measure after changing discovery, cache keys, or Nix adapter behavior.

**Debug (quick smoke):**

```bash
cargo build -p nxr-cli --quiet
# optional: clear ~/.cache/nxr or ~/Library/Caches/nxr
./target/debug/nxr --refresh-discovery -q list
./target/debug/nxr -q list
```

**Release (regression harness):**

```bash
./scripts/perf/measure-release.sh
./scripts/perf/measure-release.sh --enforce   # fail if p50 exceeds ci-thresholds.json
```

**Extended matrix (Wave 0 scenarios + optional counters):**

```bash
./scripts/perf/measure-matrix.sh
NXR_PERF_STATS=1 ./scripts/perf/measure-matrix.sh --stats
```

| Scenario | Harness | Notes |
|---|---|---|
| Cold / warm `nxr list` | `measure-release.sh`, `measure-matrix.sh` | Isolated cache home |
| Warm app plan path (`--dry-run <app>`) | `measure-matrix.sh` | Plan/prepare only |
| Task DAG plan (small / ~10 nodes) | `measure-matrix.sh` | `task-dag`, `parallel-group` dry-run |
| Task DAG plan (100 nodes) | `nxr-task` unit test | `large_dag_schedule_within_ci_budget` (in-process) |
| Affected analysis | `measure-matrix.sh` | `fixtures/affected-deps` |
| Action-key / fingerprint warm path | `measure-fingerprint.sh` | Synthetic 500-file monorepo |
| Merkle dir digest locality (~200 files) | `nxr-core` unit test | `merkle_index::tests::large_tree_dir_digest_is_stable_and_local` |
| Workspace CAS all-hit / mixed DAG | deferred | `fixtures/workspace-cache`; Wave 1+ |
| Watch edit-to-child-start | deferred | Flaky without controlled FS events |
| High-output / process log latency | deferred | Profile `nxr-process` pipe drain separately |

The script prefers `nix build .#nxr` and falls back to `cargo build -p nxr-cli --release`. It isolates cache homes, times cold/warm `list` and warm `plan` over `NXR_PERF_RUNS` (default 5), and prints per-run wall times plus **p50**. Compare p50 across commits; use p95/max to spot filesystem or Nix daemon outliers.

Suggested release p50 targets on a local SSD (order-of-magnitude; see also nxr-next performance foundations):

| Scenario | p50 target |
|---|---:|
| Warm `nxr list` (small fixture) | < 25 ms |
| Warm `nxr plan <app>` | < 75 ms |
| Warm `nxr` plan/prepare before child spawn (dry-run, cache warm) | < 50 ms (tens of ms north star) |

CI hosted-runner ceilings (see `scripts/perf/ci-thresholds.json`) are intentionally looser so the gate catches order-of-magnitude regressions without flaking on noisy VMs.

High-file-count fingerprint warm paths (index reuse, no rewrite) are asserted by
`cargo test -p nxr-completion --lib fingerprint::tests::synthetic_monorepo_warm_fingerprint_scales`
or `./scripts/perf/measure-fingerprint.sh`.

## Interpretation

- Prefer cache hits for interactive listing and completion; use `--refresh-discovery` when flake inputs change. Editing imported `.nix` files (byte content, or metadata that breaks the inode/size/mtime reuse gate) or declared `discoveryInputs` under the flake root invalidates the cache without touching `flake.nix`.
- Prefer the bare-app fast path and once-per-run `WorkspaceSnapshot` so task DAGs do not multiply `flake show`.
- Do not compare `nxr test` wall time to runner overhead — almost all of it is nextest / Nix build of the `test` app.
- Release (`nix build .#nxr`) binaries are typically faster than debug builds; treat the table as order-of-magnitude guidance.
