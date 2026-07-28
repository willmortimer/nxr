# Release-mode performance harness

Black-box timing for `nxr` using a **release** binary. Prefer the Nix-built
artifact; fall back to `cargo build --release` when Nix is unavailable.

## Prerequisites

- `nix` (recommended) or Rust toolchain
- `/usr/bin/time` (macOS) or GNU `time` on Linux
- `jq` or `python3` when enforcing thresholds

## Build the binary

```bash
# Preferred: pinned release from this flake
nix build .#nxr --no-link --print-out-paths

# Fallback when Nix is unavailable
cargo build -p nxr-cli --release
```

## Run measurements

From the repository root:

```bash
./scripts/perf/measure-release.sh
# Fail CI-style when p50 exceeds scripts/perf/ci-thresholds.json:
./scripts/perf/measure-release.sh --enforce
```

The harness isolates `HOME` / `XDG_CACHE_HOME` so cold vs warm cache behavior is
stable. It prints per-run wall times and a p50 summary.

Environment overrides:

| Variable | Default | Purpose |
|---|---|---|
| `NXR_BIN` | auto-detect | Explicit `nxr` executable |
| `NXR_PERF_RUNS` | `5` | Repetitions per scenario |
| `NXR_PERF_FIXTURE` | `fixtures/basic-apps` | Flake for list/plan scenarios |
| `NXR_PERF_PLAN_APP` | `hello` | App name for `plan` |
| `NXR_PERF_ENFORCE` | `0` | `1` → fail when p50 exceeds thresholds |
| `NXR_PERF_THRESHOLDS` | `scripts/perf/ci-thresholds.json` | Thresholds JSON |
| `NXR_PERF_MAX_WARM_LIST_S` | from JSON | Override warm list p50 ceiling (seconds) |
| `NXR_PERF_MAX_WARM_PLAN_S` | from JSON | Override warm plan p50 ceiling |
| `NXR_PERF_MAX_COLD_LIST_S` | from JSON | Override cold list p50 ceiling |

## CI gate

GitHub Actions (`ci.yml`, ubuntu + Nix latest) runs
`measure-release.sh --enforce` against the Nix-built `nxr` with
[`ci-thresholds.json`](ci-thresholds.json). Those ceilings are **order-of-magnitude**
guards for hosted runners, not the local SSD targets in
[docs/PERFORMANCE.md](../../docs/PERFORMANCE.md).

Nix **call-count** budgets (warm list: `version=0`, `help=0`, `config=0`,
`flake-show=0`) are enforced by CLI integration tests that already run in CI.

## Baseline artifact

[`baseline-aarch64-darwin.json`](baseline-aarch64-darwin.json) records sample
p50 values for local regression triage on a reference host. Update it when
remeasuring; do not treat it as the CI gate (use `ci-thresholds.json` instead).

## Fingerprint / monorepo bench

```bash
./scripts/perf/measure-fingerprint.sh
```

Runs the `synthetic_monorepo_warm_fingerprint_scales` unit test (500 `.nix`
files): warm path must re-read zero file bytes and skip index rewrite.

## Merkle directory locality

```bash
cargo test -p nxr-core --lib merkle_index::tests::large_tree_dir_digest_is_stable_and_local
```

Bounded ~200-file tree: edit under one package must not change an unrelated
package directory digest after `invalidate_paths` ([ADR-0156](../../docs/adr/0156-merkle-affected-index.md)).

## Extended scenario matrix

```bash
./scripts/perf/measure-matrix.sh
./scripts/perf/measure-matrix.sh --stats   # also emit NXR_PERF_STATS JSON per run
```

Covers task DAG dry-run, affected analysis, and warm app plan paths in addition
to cold/warm list. See [docs/PERFORMANCE.md](../../docs/PERFORMANCE.md) for the
full scenario table, north-star targets, and deferred cases.
