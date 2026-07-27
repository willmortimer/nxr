#!/usr/bin/env bash
# Synthetic high-file-count fingerprint warm-path bench (unit-test backed).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

echo "Running synthetic monorepo fingerprint warm-path assertions…"
cargo test -p nxr-completion --lib fingerprint::tests::synthetic_monorepo_warm_fingerprint_scales -- --nocapture
echo
echo "Also covered by the same suite:"
echo "  - warm_fingerprint_skips_unchanged_index_rewrite"
echo "  - discovery_inputs_warm_path_skips_reread_and_rewrite"
echo
echo "Local SSD wall-time guidance remains in docs/PERFORMANCE.md;"
echo "CI latency ceilings use scripts/perf/measure-release.sh --enforce."
