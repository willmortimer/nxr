#!/usr/bin/env bash
# Record docs/demo GIFs from VHS tapes (run from anywhere).
# Usage:
#   ./docs/demo/record.sh           # all tapes
#   ./docs/demo/record.sh nxr       # docs/demo/nxr.tape → nxr.gif
#   ./docs/demo/record.sh tui ui wizard
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

if ! command -v vhs >/dev/null 2>&1; then
  echo "vhs not found. Install: brew install vhs  (or see https://github.com/charmbracelet/vhs)" >&2
  exit 1
fi

cargo build -p nxr-cli

targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
  targets=(nxr tui ui wizard)
fi

for name in "${targets[@]}"; do
  tape="docs/demo/nxr.tape"
  case "$name" in
    nxr) tape="docs/demo/nxr.tape" ;;
    tui) tape="docs/demo/nxr-tui.tape" ;;
    ui) tape="docs/demo/nxr-ui.tape" ;;
    wizard) tape="docs/demo/nxr-wizard.tape" ;;
    *)
      if [[ -f "docs/demo/${name}.tape" ]]; then
        tape="docs/demo/${name}.tape"
      elif [[ -f "docs/demo/nxr-${name}.tape" ]]; then
        tape="docs/demo/nxr-${name}.tape"
      else
        echo "unknown demo target: $name" >&2
        exit 1
      fi
      ;;
  esac
  echo "Recording $tape …"
  vhs "$tape"
done

ls -lh docs/demo/*.gif
