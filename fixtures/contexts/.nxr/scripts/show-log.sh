#!/usr/bin/env sh
set -eu
printf 'RUST_LOG=%s\n' "${RUST_LOG:-unset}"
