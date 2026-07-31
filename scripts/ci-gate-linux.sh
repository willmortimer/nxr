#!/usr/bin/env bash
# Run the same quality gate as GitHub Actions on Linux.
#
# Prefers the OrbStack machine `nxr-ci-linux` (ubuntu + Determinate Nix).
# Falls back to Docker (`nix/ci/Dockerfile.linux`) when OrbStack is unavailable.
#
# Usage:
#   nxr task ci-linux
#   ./scripts/ci-gate-linux.sh
#   ./scripts/ci-gate-linux.sh -- nix run .#test -- process_up_worker
#   NXR_CI_LINUX_BACKEND=docker nxr task ci-linux
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Already on Linux (e.g. GHA ubuntu): run the gate natively and stamp "linux".
if [[ "$(uname -s)" == "Linux" ]]; then
  if [[ $# -eq 0 ]]; then
    set -- nix run .#ci-gate -L
  elif [[ "${1:-}" == "--" ]]; then
    shift
  fi
  export NXR_CI_LINUX=1
  unset NXR_DEV_SHELL || true
  export GIT_CONFIG_GLOBAL=/dev/null
  export GIT_CONFIG_SYSTEM=/dev/null
  "$@"
  exec "$root/scripts/release-gates.sh" stamp linux
fi

machine="${NXR_CI_LINUX_MACHINE:-nxr-ci-linux}"
backend="${NXR_CI_LINUX_BACKEND:-auto}"
image="${NXR_CI_LINUX_IMAGE:-nxr-ci-linux:local}"
platform="${NXR_CI_LINUX_PLATFORM:-}"
nix_volume="${NXR_CI_LINUX_NIX_VOLUME:-nxr-ci-linux-nix}"
dockerfile="$root/nix/ci/Dockerfile.linux"

if [[ $# -eq 0 ]]; then
  set -- nix run .#ci-gate -L
elif [[ "${1:-}" == "--" ]]; then
  shift
fi

# Quote args for remote bash -lc.
remote_cmd=$(printf '%q ' "$@")

ensure_orb_machine() {
  local orb_bin
  orb_bin="$(command -v orb 2>/dev/null || true)"
  if [[ -z "$orb_bin" && -x /opt/homebrew/bin/orb ]]; then
    orb_bin=/opt/homebrew/bin/orb
  fi
  [[ -n "$orb_bin" ]] || return 1

  if ! "$orb_bin" list 2>/dev/null | awk '{print $1}' | grep -qx "$machine"; then
    echo "info: creating OrbStack machine $machine (ubuntu:24.04 arm64)…" >&2
    orbctl create -a arm64 --cpus 6 --memory 8G --disk 80G ubuntu:24.04 "$machine"
  fi

  # Ensure machine is up.
  "$orb_bin" start "$machine" >/dev/null 2>&1 || true

  # Install Determinate Nix + git if missing.
  "$orb_bin" -m "$machine" bash -lc '
    set -euo pipefail
    export DEBIAN_FRONTEND=noninteractive
    export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
    if ! command -v git >/dev/null 2>&1; then
      sudo apt-get update -qq
      sudo apt-get install -y -qq ca-certificates curl git xz-utils build-essential >/dev/null
    fi
    if ! command -v nix >/dev/null 2>&1; then
      curl --proto "=https" --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
        | sh -s -- install linux --no-confirm \
          --extra-conf "experimental-features = nix-command flakes"
    fi
    . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null || true
    nix --version >/dev/null
  '
}

run_orb() {
  local orb_bin
  orb_bin="$(command -v orb 2>/dev/null || echo /opt/homebrew/bin/orb)"
  echo "info: OrbStack machine $machine → $*" >&2
  "$orb_bin" -m "$machine" -w "$root" bash -lc "
    set -euo pipefail
    export PATH=\"/nix/var/nix/profiles/default/bin:\$HOME/.nix-profile/bin:\$PATH\"
    . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh 2>/dev/null || true
    unset NXR_DEV_SHELL || true
    export NXR_CI_LINUX=1
    export GIT_CONFIG_GLOBAL=/dev/null
    export GIT_CONFIG_SYSTEM=/dev/null
    cd $(printf '%q' "$root")
    $remote_cmd
  "
}

resolve_docker() {
  if command -v docker >/dev/null 2>&1; then
    command -v docker
    return 0
  fi
  for candidate in /usr/local/bin/docker /opt/homebrew/bin/docker; do
    if [[ -x "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

run_docker() {
  local DOCKER
  DOCKER="$(resolve_docker)" || {
    echo "error: docker not found (install/start OrbStack)" >&2
    exit 1
  }

  if ! "$DOCKER" info >/dev/null 2>&1; then
    if command -v orb >/dev/null 2>&1 || [[ -x /opt/homebrew/bin/orb ]]; then
      local orb_bin
      orb_bin="$(command -v orb 2>/dev/null || echo /opt/homebrew/bin/orb)"
      echo "info: starting OrbStack…" >&2
      "$orb_bin" start >/dev/null 2>&1 || true
      for _ in $(seq 1 30); do
        if "$DOCKER" info >/dev/null 2>&1; then
          break
        fi
        sleep 1
      done
    fi
  fi

  if ! "$DOCKER" info >/dev/null 2>&1; then
    echo "error: docker daemon not reachable" >&2
    exit 1
  fi

  if [[ -z "$platform" ]]; then
    case "$(uname -m)" in
      arm64 | aarch64) platform=linux/arm64 ;;
      *) platform=linux/amd64 ;;
    esac
  fi

  echo "info: building $image ($platform)" >&2
  "$DOCKER" build --platform "$platform" -t "$image" -f "$dockerfile" "$root/nix/ci"
  "$DOCKER" volume create "$nix_volume" >/dev/null

  echo "info: docker run $image → $*" >&2
  "$DOCKER" run --rm --platform "$platform" \
    -v "$root:/src:rw" \
    -v "$nix_volume:/nix" \
    -w /src \
    -e "NIX_CONFIG=experimental-features = nix-command flakes" \
    -e NXR_DEV_SHELL= \
    -e NXR_CI_LINUX=1 \
    "$image" "$@"
}

case "$backend" in
  orb)
    ensure_orb_machine
    run_orb "$@"
    ;;
  docker)
    run_docker "$@"
    ;;
  auto)
    if ensure_orb_machine 2>/tmp/nxr-ci-linux-orb-ensure.log; then
      run_orb "$@"
    else
      echo "info: OrbStack machine unavailable; falling back to Docker" >&2
      cat /tmp/nxr-ci-linux-orb-ensure.log >&2 || true
      run_docker "$@"
    fi
    ;;
  *)
    echo "error: unknown NXR_CI_LINUX_BACKEND=$backend (use auto|orb|docker)" >&2
    exit 1
    ;;
esac

exec "$root/scripts/release-gates.sh" stamp linux
