#!/usr/bin/env bash
set -euo pipefail
if [ -z "${NXR_FIXTURE_SHELL_MARKER:-}" ]; then
  echo "missing shell marker" >&2
  exit 1
fi
echo "script-marker:${NXR_FIXTURE_SHELL_MARKER}"
