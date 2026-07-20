# Performance (V1 / V2)

Baselines for the runner. App **execution** time is dominated by `nix run` and the app itself; `nxr` overhead is discovery, planning, and process supervision.

## Nix call budgets

| Path | Expected Nix invocations | Notes |
|---|---|---|
| Bare `nxr <app>` / `nxr run <app>` | **1×** `nix run`; **0×** `flake show` | Fast path; optional `flake show` only after failure for suggestions |
| Adapter init | **1×** `nix eval` (`currentSystem`) + capability probes (`--version`, config/help) | Shared via `WorkspaceSnapshot` / `NixAdapter`; not repeated per task node |
| `nxr task` with **N** nodes | **N×** `nix run` + **O(1)** discovery | One `flake show` (apps) + one task `eval` (or warm combined cache); **not** N× `flake show` |
| `nxr list --refresh-discovery` | Dominated by `nix flake show` | Catalog commands still discover |

Instrumented integration tests wrap `NXR_NIX` with a counting shim to assert these budgets.

## Budgets

| Path | Budget | Notes |
|---|---|---|
| Interactive completion (`nxr __complete apps`) | ≤ **500 ms** cold discovery wait | [`DISCOVERY_TIMEOUT`](../crates/nxr-completion/src/dynamic.rs); empty candidates on timeout |
| Warm `nxr list` (cache hit) | Interactive (tens of ms) | Combined apps+tasks discovery cache |
| Cold `nxr list --refresh-discovery` | Dominated by `nix flake show` + one task eval | Nix eval/store caches still apply |

Discovery cache **schema v3** invalidates on:

- **Content** of every `*.nix` file under the local flake root (path-scoped walk), plus `flake.lock` when present — not mtime/size, and not arbitrary non-Nix sources
- **Nix identity**: canonical Nix executable path + version string
- **Discovery schema version** (`nxr.<system>` / task document major)
- **Sorted `discoveryInputs`**: content hashes of paths declared via `perSystem.nxr.discoveryInputs` (emitted on `nxr.<system>.discoveryInputs`; hashed on store/load without a second eval on warm hits)
- **TTL backstop**: default 24h (`NXR_CACHE_TTL_SECS`; `0` disables)

Built-in ignores cover `.git`, `result`, `target`, and similar trees. Set `NXR_CACHE_FINGERPRINT_IGNORE` to a colon-separated list of globs to skip huge vendored `.nix` trees. Remote flakes are never cached.

## Measured baselines

Host: `aarch64-darwin` (Apple Silicon), macOS 26.5.1, Nix 2.34.7. Binary: `cargo build -p nxr-cli` (debug). Timings via `/usr/bin/time -p`, three runs, quiet mode where applicable. Measured 2026-07-18.

| Scenario | Avg wall time | Observations |
|---|---|---|
| Cold `nxr --refresh-discovery list` (this repo) | **0.62 s** | First refresh ~1.5 s; later refreshes ~0.17 s (Nix evaluation cache) |
| Warm `nxr list` (this repo) | **0.05 s** | Discovery cache hit |
| Cold `nxr --flake ./fixtures/basic-apps --refresh-discovery list` | **0.18 s** | Small fixture flake |
| Warm `nxr --flake ./fixtures/basic-apps list` | **0.05 s** | Cache hit |
| Warm `nxr __complete apps` | **0.05 s** | Within completion budget |
| `nxr plan test` | **0.17 s** | Resolve + plan; no app execution |

Re-measure after changing discovery, cache keys, or Nix adapter behavior:

```bash
cargo build -p nxr-cli --quiet
# optional: clear ~/.cache/nxr or ~/Library/Caches/nxr
./target/debug/nxr --refresh-discovery -q list
./target/debug/nxr -q list
```

## Interpretation

- Prefer cache hits for interactive listing and completion; use `--refresh-discovery` when flake inputs change. Editing imported `.nix` files (content) or declared `discoveryInputs` under the flake root invalidates the cache without touching `flake.nix`.
- Prefer the bare-app fast path and once-per-run `WorkspaceSnapshot` so task DAGs do not multiply `flake show`.
- Do not compare `nxr test` wall time to runner overhead — almost all of it is nextest / Nix build of the `test` app.
- Release (`nix build .#nxr`) binaries are typically faster than debug builds; treat the table as order-of-magnitude guidance.
