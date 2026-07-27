# Performance (V1 / V2)

Baselines for the runner. App **execution** time is dominated by `nix run` and the app itself; `nxr` overhead is discovery, planning, and process supervision.

## Nix call budgets

| Path | Expected Nix invocations | Notes |
|---|---|---|
| Bare `nxr <app>` / `nxr run <app>` (success or ordinary app failure) | **exactly 1×** `nix` (`nix run`); **0×** probes / `flake show` | Locate-only prepare; no `currentSystem` / capability probes unless `--offline` / `--accept-flake-config`; TTY stderr is inherited (no capture) |
| Bare app missing installable (non-TTY stderr) | **1×** `nix run` + optional diagnostic discovery | Bounded stderr tail (~128 KiB); suggestion discovery only when stderr indicates installable-resolution failure |
| Bare app on a TTY | **1×** `nix run`; inherit stderr | Prefer transparent rendering over typo suggestions |
| Adapter init (list/task/doctor) | **1×** `nix eval` (`currentSystem`) + capability probes (`--version`, config/help) | Shared via `WorkspaceSnapshot` / `NixAdapter`; warm capability cache skips all probes when the environment digest matches |
| `nxr task` with **N** nodes | **N×** `nix run` + **O(1)** discovery | One `flake show` (apps) + one task `eval` (or warm combined cache); **not** N× `flake show` |
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

Capability cache (schema **v3**) invalidates on Nix executable identity (canonical path + device/inode + size + mtime) **and** an environment digest (`NIX_CONFIG` / `NIX_USER_CONF_FILES` / `NIX_CONF_DIR` plus `nix config show --json` at store time), with a 7-day TTL backstop (`NXR_CAPABILITY_CACHE_TTL_SECS`; `0` disables). Warm hits with a matching environment digest skip all capability probes (version, config, help, and system). When the binary cache is warm but the environment digest changed, only `nix config show` is re-probed. Set `NXR_CAPABILITY_CACHE=off` to bypass. `nxr cache clear` removes capability entries alongside discovery cache.

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

The script prefers `nix build .#nxr` and falls back to `cargo build -p nxr-cli --release`. It isolates cache homes, times cold/warm `list` and warm `plan` over `NXR_PERF_RUNS` (default 5), and prints per-run wall times plus **p50**. Compare p50 across commits; use p95/max to spot filesystem or Nix daemon outliers.

Suggested release p50 targets on a local SSD (order-of-magnitude; see also nxr-next performance foundations):

| Scenario | p50 target |
|---|---:|
| Warm `nxr list` (small fixture) | < 25 ms |
| Warm `nxr plan <app>` | < 75 ms |

CI hosted-runner ceilings (see `scripts/perf/ci-thresholds.json`) are intentionally looser so the gate catches order-of-magnitude regressions without flaking on noisy VMs.

High-file-count fingerprint warm paths (index reuse, no rewrite) are asserted by
`cargo test -p nxr-completion --lib fingerprint::tests::synthetic_monorepo_warm_fingerprint_scales`
or `./scripts/perf/measure-fingerprint.sh`.

## Interpretation

- Prefer cache hits for interactive listing and completion; use `--refresh-discovery` when flake inputs change. Editing imported `.nix` files (byte content, or metadata that breaks the inode/size/mtime reuse gate) or declared `discoveryInputs` under the flake root invalidates the cache without touching `flake.nix`.
- Prefer the bare-app fast path and once-per-run `WorkspaceSnapshot` so task DAGs do not multiply `flake show`.
- Do not compare `nxr test` wall time to runner overhead — almost all of it is nextest / Nix build of the `test` app.
- Release (`nix build .#nxr`) binaries are typically faster than debug builds; treat the table as order-of-magnitude guidance.
