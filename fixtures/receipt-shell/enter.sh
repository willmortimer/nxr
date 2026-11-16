#!/bin/sh
# Enter the fixture toolchain. Tests compare this process with the receipt.
root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
export PATH="$root/bin:$PATH"
export RUSTC_WRAPPER="$root/bin/wrapper"
export CARGO_TARGET_DIR="$root/target"
export NXR_CACHE_IDENTITY="fixture-cache"
exec "$@"
