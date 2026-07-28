#!/usr/bin/env bash
set -euo pipefail
printf 'script-hello'
if [[ $# -gt 0 ]]; then
  printf ' '
  printf '%s' "$1"
  shift
  while [[ $# -gt 0 ]]; do
    printf '/%s' "$1"
    shift
  done
fi
printf '\n'
