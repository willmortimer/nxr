#!/usr/bin/env bash
# Run one workflow leaf and write ci-run-metadata-v2.
# A checksum match is not signature verification. Cosign must accept the bundle.
set -euo pipefail

task="${INPUT_TASK:-finish}"
root="${INPUT_WORKFLOW_ROOT:-.github/fixtures/wave3}"
version="${INPUT_VERSION:-3.7.1}"
nxr_bin="${INPUT_NXR:-${NXR:-}}"
require_operation_id="${INPUT_REQUIRE_OPERATION_ID:-true}"

verify_sha256() {
  local file="$1"
  local sums="$2"
  local base expected actual
  base="$(basename "$file")"
  expected="$(awk -v name="$base" '$2 == name { print $1; exit }' "$sums")"
  if [[ -z "$expected" ]]; then
    echo "SHA256SUMS has no entry for $base" >&2
    return 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    echo "$expected  $base" | (cd "$(dirname "$file")" && sha256sum -c -)
  else
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    if [[ "$actual" != "$expected" ]]; then
      echo "checksum mismatch for $base" >&2
      return 1
    fi
  fi
}

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
    curl -fsSL -o "$work/$archive.sigstore.json" "$base/$archive.sigstore.json"
    verify_sha256 "$work/$archive" "$work/SHA256SUMS"
    if ! command -v cosign >/dev/null 2>&1; then
      echo "cosign is required to verify the NXR release; a checksum match is not enough" >&2
      exit 1
    fi
    cosign verify-blob \
      --bundle "$work/$archive.sigstore.json" \
      --certificate-identity-regexp '^https://github\.com/willmortimer/nxr/\.github/workflows/release\.yml@refs/tags/v' \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      "$work/$archive"
    tar -xzf "$work/$archive" -C "$work"
    nxr_bin="$(find "$work" -type f -name nxr -perm -111 | head -n 1)"
  fi
fi

if [[ -z "$nxr_bin" || ! -x "$nxr_bin" ]]; then
  echo "nxr binary is missing" >&2
  exit 1
fi

if ! "$nxr_bin" workflow spec --help >/dev/null 2>&1; then
  echo "nxr at $nxr_bin does not provide workflow spec" >&2
  if [[ "$require_operation_id" == "true" ]]; then
    exit 1
  fi
fi

cd "${GITHUB_WORKSPACE:-.}"
cd "$root"
set +e
"$nxr_bin" workflow run "$task"
code=$?
set -e

operation_id=""
provenance_ok=0
if spec_json="$("$nxr_bin" workflow spec "$task" 2>/dev/null)"; then
  operation_id="$(printf '%s' "$spec_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')"
  if [[ "$operation_id" =~ ^op_[0-9a-f]{32}$ ]]; then
    provenance_ok=1
  else
    operation_id=""
  fi
fi

provider="${CI_PROVIDER:-github}"
run_id="${GITHUB_RUN_ID:-local}"
job="${GITHUB_JOB:-local}"
attempt="${GITHUB_RUN_ATTEMPT:-1}"
url=""
if [[ -n "${GITHUB_SERVER_URL:-}" && -n "${GITHUB_REPOSITORY:-}" && -n "${GITHUB_RUN_ID:-}" ]]; then
  url="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
fi
out="${GITHUB_WORKSPACE:-$PWD}/ci-run-metadata.json"
bash "$(dirname "$0")/write-metadata.sh" "$out" "$provider" "$run_id" "$job" "$attempt" "$url" "$operation_id" "$code"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "exit-code=$code" >> "$GITHUB_OUTPUT"
  echo "metadata=$out" >> "$GITHUB_OUTPUT"
fi

if [[ "$require_operation_id" == "true" && "$provenance_ok" != "1" ]]; then
  echo "operation provenance is required and workflow spec did not return an operation id" >&2
  exit 1
fi
exit "$code"
