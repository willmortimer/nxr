# Stargate WASM artifact-aware reuse DAG (S4)

**Status:** design + task sketch. Baseline desktop pin remains `efea14bb`
until re-qualified. NXR v3.9.0 immutable.

## Goal

One WASM publish → digest pin → desktop/web consumers materialize or rebuild
with an explicit reason and a phase timing report.

## Task graph (NXR)

```text
wasm-publish          # produce content digest D
  ├─ desktop-materialize  # reuse D or rebuild(reason=…)
  └─ web-materialize      # reuse D or rebuild(reason=…)
phase-timing-report       # dependsOn both; emit JSON
```

Leaves stay flake apps (`apps.<system>.*`). Tasks only coordinate.

## Reuse rules

1. If local/cache artifact matches digest `D` → `reuse`.
2. Else rebuild with `reason` in {`missing`, `digest_mismatch`, `forced`}.
3. Laptop Nix store presence is **not** provenance; NXB on
   `windows-pc/nixdev` owns Linux Cloud/WASM builder receipts.

## Phase timing report schema (sketch)

```json
{
  "schema": "wasm-reuse-timing-v1",
  "wasmDigest": "sha256:…",
  "phases": [
    {"name": "wasm-publish", "ms": 0, "outcome": "publish"},
    {"name": "desktop-materialize", "ms": 0, "outcome": "reuse|rebuild", "reason": null}
  ]
}
```

## Non-goals

- Tracking NixPlane/NXR main from consumer flakes
- Claiming SYS-5 cutover proof from a desktop WASM rebuild
