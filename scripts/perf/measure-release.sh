#!/usr/bin/env bash
# Time warm/cold nxr list and plan with a release binary (black-box harness).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

runs="${NXR_PERF_RUNS:-5}"
fixture="${NXR_PERF_FIXTURE:-fixtures/basic-apps}"
plan_app="${NXR_PERF_PLAN_APP:-hello}"

resolve_nxr_bin() {
  if [[ -n "${NXR_BIN:-}" ]]; then
    echo "$NXR_BIN"
    return
  fi
  if command -v nix >/dev/null 2>&1; then
    local out
    if out="$(nix build .#nxr --no-link --print-out-paths 2>/dev/null)"; then
      echo "${out%/}/bin/nxr"
      return
    fi
  fi
  cargo build -p nxr-cli --release --quiet
  echo "$root/target/release/nxr"
}

time_one() {
  local label="$1"
  shift
  local -a samples=()
  local i wall
  for ((i = 1; i <= runs; i++)); do
    wall="$(/usr/bin/time -p "$@" 2>&1 >/dev/null | awk '/^real / {print $2}')"
    samples+=("$wall")
    printf '%s run %d: %ss\n' "$label" "$i" "$wall"
  done
  printf '%s p50: %ss\n' "$label" "$(printf '%s\n' "${samples[@]}" | sort -n | awk -v n="$runs" 'NR==int((n+1)/2) {print $1}')"
}

nxr_bin="$(resolve_nxr_bin)"
if [[ ! -x "$nxr_bin" ]]; then
  echo "nxr binary not executable: $nxr_bin" >&2
  exit 1
fi

echo "nxr: $nxr_bin"
echo "runs: $runs fixture: $fixture plan_app: $plan_app"
echo

# Cold list: refresh discovery each run.
time_one "cold list" "$nxr_bin" --flake "$fixture" --refresh-discovery -q list

# Warm list: discovery cache hit.
time_one "warm list" "$nxr_bin" --flake "$fixture" -q list

# Warm plan: resolve + plan only (no app execution).
time_one "warm plan" "$nxr_bin" --flake "$fixture" plan "$plan_app"
