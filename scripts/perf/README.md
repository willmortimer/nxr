# Release-mode performance harness

Black-box timing for `nxr` using a **release** binary. Prefer the Nix-built
artifact; fall back to `cargo build --release` when Nix is unavailable.

## Prerequisites

- `nix` (recommended) or Rust toolchain
- `/usr/bin/time` (macOS) or GNU `time` on Linux

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
```

Environment overrides:

| Variable | Default | Purpose |
|---|---|---|
| `NXR_BIN` | auto-detect | Explicit `nxr` executable |
| `NXR_PERF_RUNS` | `5` | Repetitions per scenario |
| `NXR_PERF_FIXTURE` | `fixtures/basic-apps` | Flake for list/plan scenarios |
| `NXR_PERF_PLAN_APP` | `hello` | App name for `plan` |

The script prints per-run wall times and a p50 summary. Compare p50 across
commits; use p95/max to spot filesystem or Nix daemon outliers (see
[docs/PERFORMANCE.md](../../docs/PERFORMANCE.md)).

## Baseline artifact

[`baseline-aarch64-darwin.json`](baseline-aarch64-darwin.json) records sample
p50 values for regression triage. Update it when remeasuring on a reference
host; do not treat it as a CI gate.
