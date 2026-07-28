#!/usr/bin/env bash
# Extended performance matrix: wall times + optional NXR_PERF_STATS counters.
#
# Stable scenarios run by default. Deferred scenarios are documented stubs (watch,
# 100-node DAG flake, high-output latency) — see docs/PERFORMANCE.md.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

runs="${NXR_PERF_RUNS:-3}"
collect_stats="${NXR_PERF_STATS:-0}"
fixture="${NXR_PERF_FIXTURE:-fixtures/basic-apps}"
plan_app="${NXR_PERF_PLAN_APP:-hello}"

for arg in "$@"; do
  case "$arg" in
    --stats) collect_stats=1 ;;
    -h | --help)
      cat <<'EOF'
Usage: measure-matrix.sh [--stats]

Runs an extended scenario matrix (wall time p50 per scenario). With --stats,
sets NXR_PERF_STATS=1 and prints the last nxr-perf-stats JSON line per run.

Environment:
  NXR_BIN                 Explicit nxr executable
  NXR_PERF_RUNS           Repetitions per scenario (default 3)
  NXR_PERF_FIXTURE        Flake for list/plan (default fixtures/basic-apps)
  NXR_PERF_PLAN_APP       App for plan (default hello)
  NXR_PERF_STATS          If 1, collect counter JSON (same as --stats)

Deferred (documented only — not executed here):
  watch edit-to-child-start, 100-node task DAG flake, high-output log latency
EOF
      exit 0
      ;;
  esac
done

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
  local i wall stats_line
  for ((i = 1; i <= runs; i++)); do
    local -a cmd=( "$@" )
    if [[ "$collect_stats" == "1" ]]; then
      cmd=( env NXR_PERF_STATS=1 "${cmd[@]}" )
    fi
    if [[ "$collect_stats" == "1" ]]; then
      local output
      output="$(/usr/bin/time -p "${cmd[@]}" 2>&1 >/dev/null || true)"
      wall="$(printf '%s\n' "$output" | awk '/^real / {print $2}')"
      stats_line="$(printf '%s\n' "$output" | awk -F': ' '/^nxr-perf-stats: / {print $2}' | tail -1)"
      if [[ -n "${stats_line:-}" ]]; then
        printf '%s run %d stats: %s\n' "$label" "$i" "$stats_line" >&2
      fi
    else
      wall="$(/usr/bin/time -p "${cmd[@]}" 2>&1 >/dev/null | awk '/^real / {print $2}')"
    fi
    samples+=("$wall")
    printf '%s run %d: %ss\n' "$label" "$i" "$wall" >&2
  done
  local p50
  p50="$(printf '%s\n' "${samples[@]}" | sort -n | awk -v n="$runs" 'NR==int((n+1)/2) {print $1}')"
  printf '%s p50: %ss\n' "$label" "$p50" >&2
  printf '%s\n' "$p50"
}

nxr_bin="$(resolve_nxr_bin)"
if [[ ! -x "$nxr_bin" ]]; then
  echo "nxr binary not executable: $nxr_bin" >&2
  exit 1
fi

perf_home="$(mktemp -d "${TMPDIR:-/tmp}/nxr-perf-matrix.XXXXXX")"
cleanup() { rm -rf "$perf_home"; }
trap cleanup EXIT
export HOME="$perf_home"
export XDG_CACHE_HOME="$perf_home/cache"
mkdir -p "$XDG_CACHE_HOME"

echo "nxr: $nxr_bin"
echo "runs: $runs stats: $collect_stats"
echo "cache home: $perf_home"
echo

# Prime discovery cache for warm scenarios.
"$nxr_bin" --flake "$fixture" --refresh-discovery -q list >/dev/null

cold_list="$(time_one "cold list" "$nxr_bin" --flake "$fixture" --refresh-discovery -q list)"
warm_list="$(time_one "warm list" "$nxr_bin" --flake "$fixture" -q list)"
warm_plan="$(time_one "warm plan" "$nxr_bin" --flake "$fixture" plan "$plan_app")"
warm_run_prepare="$(time_one "warm run dry-run" "$nxr_bin" --flake "$fixture" --dry-run "$plan_app")"

# Task DAG planning (small fixture, dry-run avoids Nix execution).
"$nxr_bin" --flake fixtures/task-dag --refresh-discovery -q list >/dev/null
task_dag_plan="$(time_one "task-dag dry-run" "$nxr_bin" --flake fixtures/task-dag --dry-run task ci)"

# ~10-node diamond / parallel-group planning.
"$nxr_bin" --flake fixtures/parallel-group --refresh-discovery -q list >/dev/null
parallel_plan="$(time_one "parallel-group dry-run" "$nxr_bin" --flake fixtures/parallel-group --dry-run task join)"

# Affected analysis (small fixture).
"$nxr_bin" --flake fixtures/affected-deps --refresh-discovery -q list >/dev/null
affected="$(time_one "affected list" "$nxr_bin" --flake fixtures/affected-deps affected --json)"

# Action-key / fingerprint warm path (unit-test backed).
echo "fingerprint warm path: cargo test -p nxr-completion --lib fingerprint::tests::synthetic_monorepo_warm_fingerprint_scales" >&2
cargo test -p nxr-completion --lib fingerprint::tests::synthetic_monorepo_warm_fingerprint_scales --quiet

echo
echo "summary p50:"
echo "  cold_list=${cold_list}s warm_list=${warm_list}s warm_plan=${warm_plan}s"
echo "  warm_run_dry_run=${warm_run_prepare}s task_dag_dry_run=${task_dag_plan}s"
echo "  parallel_group_dry_run=${parallel_plan}s affected=${affected}s"
echo
echo "Deferred scenarios (manual / future harness):"
echo "  - 100-node task DAG: nxr-task scheduler::large_dag_schedule_within_ci_budget (in-process)"
echo "  - workspace CAS all-hit/mixed DAG: fixtures/workspace-cache (Wave 1+)"
echo "  - watch edit-to-child-start: flaky without controlled FS events"
echo "  - high-output / process logs latency: profile nxr-process pipe drain separately"
