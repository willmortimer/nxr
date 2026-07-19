#!/usr/bin/env bash
# Record docs/demo/nxr.gif from docs/demo/nxr.tape (run from anywhere).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

if ! command -v vhs >/dev/null 2>&1; then
  echo "vhs not found. Install: brew install vhs  (or see https://github.com/charmbracelet/vhs)" >&2
  exit 1
fi

cargo build -p nxr-cli
vhs docs/demo/nxr.tape
ls -lh docs/demo/nxr.gif
