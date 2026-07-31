#!/usr/bin/env bash
# Stamp / require local release quality gates (host + Linux).
#
# Successful `nxr task ci` / `nxr task ci-linux` (or flake escape hatches
# `.#ci-gate` / `.#ci-gate-linux`) write stamps keyed by HEAD.
# `scripts/release.sh --execute` refuses to tag unless both stamps exist.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dir="${NXR_RELEASE_GATE_DIR:-$root/.nxr/release-gates}"

usage() {
  cat <<'EOF'
Usage: release-gates.sh stamp <host|linux>
       release-gates.sh require
       release-gates.sh status

  stamp     Record a successful gate for HEAD.
  require   Exit 0 only when host + linux stamps exist for HEAD.
  status    Print stamp presence for HEAD (always exit 0).
EOF
}

head_sha() {
  git -C "$root" rev-parse HEAD
}

stamp_path() {
  local kind="$1"
  local sha
  sha="$(head_sha)"
  printf '%s/%s.%s' "$dir" "$kind" "$sha"
}

cmd="${1:-}"
case "$cmd" in
  -h | --help | "")
    usage
    exit 0
    ;;
  stamp)
    kind="${2:-}"
    if [[ "$kind" != "host" && "$kind" != "linux" ]]; then
      echo "error: stamp requires host|linux" >&2
      usage >&2
      exit 2
    fi
    mkdir -p "$dir"
    path="$(stamp_path "$kind")"
    date -u +%Y-%m-%dT%H:%M:%SZ >"$path"
    echo "ok: stamped release gate ${kind} for $(head_sha) → ${path#"$root"/}"
    ;;
  require)
    sha="$(head_sha)"
    missing=0
    for kind in host linux; do
      path="$(stamp_path "$kind")"
      if [[ -f "$path" ]]; then
        echo "ok: ${kind} gate stamp for ${sha} ($(tr -d '\n' <"$path"))"
      else
        echo "error: missing ${kind} gate stamp for HEAD ${sha}" >&2
        missing=1
      fi
    done
    if [[ "$missing" -ne 0 ]]; then
      cat >&2 <<EOF
error: --execute requires both local CI gate stamps for this commit:
  nxr task ci
  nxr task ci-linux
(or: nix run .#ci-gate && nix run .#ci-gate-linux)
(or pass --skip-gates to break glass — not for normal releases)
EOF
      exit 1
    fi
    ;;
  status)
    sha="$(head_sha)"
    for kind in host linux; do
      path="$(stamp_path "$kind")"
      if [[ -f "$path" ]]; then
        echo "${kind}: ok ($(tr -d '\n' <"$path"))"
      else
        echo "${kind}: missing"
      fi
    done
    ;;
  *)
    echo "error: unknown command: $cmd" >&2
    usage >&2
    exit 2
    ;;
esac
