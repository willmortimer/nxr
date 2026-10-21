#!/usr/bin/env bash
# Time warm nxr list and nxr plan against a real multi-repo checkout.
#
# This is the ecosystem regression fixture. It is not a CI gate: GitHub runners
# do not have the Lattice checkout, and ci-thresholds.json applies to
# fixtures/basic-apps. A slow result here is a follow-up, not a fixture-harness
# failure.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

runs="${NXR_PERF_RUNS:-5}"
ecosystem="${NXR_PERF_ECOSYSTEM:-}"
if [[ -z "$ecosystem" ]]; then
  candidate="$(cd "$root/.." && pwd)/lattice-ecosystem"
  if [[ -f "$candidate/flake.nix" ]]; then
    ecosystem="$candidate"
  fi
fi
if [[ -z "$ecosystem" || ! -f "$ecosystem/flake.nix" ]]; then
  echo "set NXR_PERF_ECOSYSTEM to a flake checkout (expected ../lattice-ecosystem)" >&2
  exit 1
fi

if [[ -n "${NXR_BIN:-}" ]]; then
  nxr_bin="$NXR_BIN"
elif command -v nix >/dev/null 2>&1; then
  out="$(nix build .#nxr --no-link --print-out-paths)"
  nxr_bin="${out%/}/bin/nxr"
else
  cargo build -p nxr-cli --release --quiet
  nxr_bin="$root/target/release/nxr"
fi

plan_target="${NXR_PERF_PLAN_TARGET:-ok}"

echo "nxr: $nxr_bin"
echo "ecosystem: $ecosystem"
echo "plan target: $plan_target"
"$nxr_bin" --version
echo

python3 - "$nxr_bin" "$ecosystem" "$plan_target" "$runs" <<'PY'
import math
import os
import subprocess
import sys
import time

nxr, ecosystem, plan_target, runs = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
env = os.environ.copy()
env["NXR_PERF_STATS"] = "1"


def once(args):
    started = time.perf_counter()
    proc = subprocess.run(
        [nxr, *args],
        cwd=ecosystem,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000
    stats = ""
    for line in proc.stderr.splitlines():
        if "nxr-perf-stats" in line:
            stats = line.split("nxr-perf-stats:", 1)[-1].strip()
    return proc.returncode, elapsed_ms, stats


def percentile_ms(ordered, pct):
    if not ordered:
        return 0.0
    rank = max(1, min(len(ordered), int(math.ceil(pct / 100.0 * len(ordered)))))
    return ordered[rank - 1]


def measure(label, args):
    samples = []
    last = ""
    for index in range(1, runs + 1):
        code, elapsed_ms, stats = once(args)
        samples.append(elapsed_ms)
        last = stats
        print(f"{label} run {index}: {elapsed_ms:.1f} ms exit={code}")
    ordered = sorted(samples)
    p50 = percentile_ms(ordered, 50)
    p95 = percentile_ms(ordered, 95)
    print(f"{label} p50: {p50:.1f} ms")
    print(f"{label} p95: {p95:.1f} ms")
    if last:
        print(f"{label} stats: {last}")


once(["-q", "list"])
once(["plan", plan_target])
measure("warm list", ["-q", "list"])
measure("warm plan", ["plan", plan_target])
PY
