#!/usr/bin/env bash
# Run one workflow leaf and write ci-run-metadata.json beside the workspace.
set -euo pipefail

task="${INPUT_TASK:-finish}"
root="${INPUT_WORKFLOW_ROOT:-.github/fixtures/wave2}"
version="${INPUT_VERSION:-3.7.1}"
nxr_bin="${INPUT_NXR:-${NXR:-}}"

if [[ -z "$nxr_bin" ]]; then
  if command -v nxr >/dev/null 2>&1; then
    nxr_bin="$(command -v nxr)"
  else
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64) system="x86_64-linux" ;;
      Linux-aarch64|Linux-arm64) system="aarch64-linux" ;;
      Darwin-arm64|Darwin-aarch64) system="aarch64-darwin" ;;
      Darwin-x86_64) system="x86_64-darwin" ;;
      *)
        echo "no portable NXR archive for $(uname -s)-$(uname -m)" >&2
        exit 1
        ;;
    esac
    archive="nxr-${version}-${system}-portable.tar.gz"
    base="https://github.com/willmortimer/nxr/releases/download/v${version}"
    work="$(mktemp -d)"
    curl -fsSL -o "$work/$archive" "$base/$archive"
    curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS"
    (
      cd "$work"
      grep -F "$archive" SHA256SUMS | sha256sum -c -
    )
    tar -xzf "$work/$archive" -C "$work"
    nxr_bin="$(find "$work" -type f -name nxr -perm -111 | head -n 1)"
  fi
fi

if [[ -z "$nxr_bin" || ! -x "$nxr_bin" ]]; then
  echo "nxr binary is missing" >&2
  exit 1
fi

cd "${GITHUB_WORKSPACE:-.}"
cd "$root"
set +e
"$nxr_bin" workflow run "$task"
code=$?
set -e

operation_id=""
if spec_json="$("$nxr_bin" workflow spec "$task" 2>/dev/null)"; then
  operation_id="$(printf '%s' "$spec_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')"
fi
if [[ -z "$operation_id" ]]; then
  echo "installed nxr has no workflow spec; metadata records the idempotency key in operation_id" >&2
  operation_id="github:${GITHUB_RUN_ID:-local}:${task}"
fi

provider="${CI_PROVIDER:-github}"
job="${GITHUB_RUN_ID:-local}"
url=""
if [[ -n "${GITHUB_SERVER_URL:-}" && -n "${GITHUB_REPOSITORY:-}" && -n "${GITHUB_RUN_ID:-}" ]]; then
  url="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
fi
idem="${provider}:${job}:${task}"
out="${GITHUB_WORKSPACE:-$PWD}/ci-run-metadata.json"
bash "$(dirname "$0")/write-metadata.sh" "$out" "$provider" "$job" "$url" "$idem" "$operation_id" "$code"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "exit-code=$code" >> "$GITHUB_OUTPUT"
  echo "metadata=$out" >> "$GITHUB_OUTPUT"
fi
exit "$code"
