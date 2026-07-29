#!/usr/bin/env bash
# Time warm/cold nxr list and plan with a release binary (black-box harness).
#
# Optional gate: set NXR_PERF_ENFORCE=1 (or pass --enforce) to fail when p50
# exceeds CI/local ceilings from scripts/perf/ci-thresholds.json (or env overrides).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

runs="${NXR_PERF_RUNS:-5}"
fixture="${NXR_PERF_FIXTURE:-fixtures/basic-apps}"
plan_app="${NXR_PERF_PLAN_APP:-hello}"
thresholds_file="${NXR_PERF_THRESHOLDS:-$root/scripts/perf/ci-thresholds.json}"
enforce=0

for arg in "$@"; do
  case "$arg" in
    --enforce) enforce=1 ;;
    -h | --help)
      cat <<'EOF'
Usage: measure-release.sh [--enforce]

Environment:
  NXR_BIN                 Explicit nxr executable
  NXR_PERF_RUNS           Repetitions per scenario (default 5)
  NXR_PERF_FIXTURE        Flake path (default fixtures/basic-apps)
  NXR_PERF_PLAN_APP       App for plan (default hello)
  NXR_PERF_ENFORCE        If 1, fail when p50 exceeds thresholds
  NXR_PERF_THRESHOLDS     Thresholds JSON (default scripts/perf/ci-thresholds.json)
  NXR_PERF_MAX_WARM_LIST_S / NXR_PERF_MAX_WARM_PLAN_S / NXR_PERF_MAX_COLD_LIST_S
                          Optional numeric overrides (seconds)
EOF
      exit 0
      ;;
  esac
done

if [[ "${NXR_PERF_ENFORCE:-0}" == "1" ]]; then
  enforce=1
fi

resolve_nxr_bin() {
  if [[ -n "${NXR_BIN:-}" ]]; then
    echo "$NXR_BIN"
    return
  fi
  if command -v nix >/dev/null 2>&1; then
    local out system
    system="$(nix eval --raw --impure --expr 'builtins.currentSystem')"
    if out="$(nix build ".#packages.${system}.default" --no-link --print-out-paths 2>/dev/null)"; then
      echo "${out%/}/bin/nxr"
      return
    fi
    if out="$(nix build .#nxr --no-link --print-out-paths 2>/dev/null)"; then
      echo "${out%/}/bin/nxr"
      return
    fi
  fi
  cargo build -p nxr-cli --release --quiet
  echo "$root/target/release/nxr"
}

# Print p50 of samples on stdout; print per-run lines on stderr.
time_one() {
  local label="$1"
  shift
  local -a samples=()
  local i wall
  for ((i = 1; i <= runs; i++)); do
    wall="$(/usr/bin/time -p "$@" 2>&1 >/dev/null | awk '/^real / {print $2}')"
    samples+=("$wall")
    printf '%s run %d: %ss\n' "$label" "$i" "$wall" >&2
  done
  local p50
  p50="$(printf '%s\n' "${samples[@]}" | sort -n | awk -v n="$runs" 'NR==int((n+1)/2) {print $1}')"
  printf '%s p50: %ss\n' "$label" "$p50" >&2
  printf '%s\n' "$p50"
}

read_threshold() {
  local key="$1"
  local env_override="$2"
  if [[ -n "${env_override}" ]]; then
    printf '%s\n' "$env_override"
    return
  fi
  if [[ ! -f "$thresholds_file" ]]; then
    echo "thresholds file missing: $thresholds_file" >&2
    exit 1
  fi
  # Prefer jq when present; otherwise a tiny Python one-liner (CI has python3).
  if command -v jq >/dev/null 2>&1; then
    jq -r --arg k "$key" '.[$k] | tostring' "$thresholds_file"
  else
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' \
      "$thresholds_file" "$key"
  fi
}

assert_p50_le() {
  local label="$1"
  local p50="$2"
  local max="$3"
  awk -v p50="$p50" -v max="$max" -v label="$label" 'BEGIN {
    if ((p50 + 0) > (max + 0)) {
      printf "%s p50 %ss exceeds threshold %ss\n", label, p50, max > "/dev/stderr"
      exit 1
    }
    printf "%s p50 %ss <= %ss OK\n", label, p50, max > "/dev/stderr"
    exit 0
  }'
}

nxr_bin="$(resolve_nxr_bin)"
if [[ ! -x "$nxr_bin" ]]; then
  echo "nxr binary not executable: $nxr_bin" >&2
  exit 1
fi

# Isolate caches so cold/warm semantics are stable under CI and shared runners.
perf_home="$(mktemp -d "${TMPDIR:-/tmp}/nxr-perf.XXXXXX")"
cleanup() { rm -rf "$perf_home"; }
trap cleanup EXIT
export HOME="$perf_home"
export XDG_CACHE_HOME="$perf_home/cache"
mkdir -p "$XDG_CACHE_HOME"

echo "nxr: $nxr_bin"
echo "runs: $runs fixture: $fixture plan_app: $plan_app"
echo "cache home: $perf_home"
if [[ "$enforce" -eq 1 ]]; then
  echo "enforce: on (thresholds: $thresholds_file)"
fi
echo

cold_p50="$(time_one "cold list" "$nxr_bin" --flake "$fixture" --refresh-discovery -q list)"
warm_list_p50="$(time_one "warm list" "$nxr_bin" --flake "$fixture" -q list)"
warm_plan_p50="$(time_one "warm plan" "$nxr_bin" --flake "$fixture" plan "$plan_app")"

echo
echo "summary p50: cold_list=${cold_p50}s warm_list=${warm_list_p50}s warm_plan=${warm_plan_p50}s"

if [[ "$enforce" -eq 1 ]]; then
  max_cold="$(read_threshold cold_list_p50_max_seconds "${NXR_PERF_MAX_COLD_LIST_S:-}")"
  max_warm_list="$(read_threshold warm_list_p50_max_seconds "${NXR_PERF_MAX_WARM_LIST_S:-}")"
  max_warm_plan="$(read_threshold warm_plan_p50_max_seconds "${NXR_PERF_MAX_WARM_PLAN_S:-}")"
  assert_p50_le "cold list" "$cold_p50" "$max_cold"
  assert_p50_le "warm list" "$warm_list_p50" "$max_warm_list"
  assert_p50_le "warm plan" "$warm_plan_p50" "$max_warm_plan"
fi
