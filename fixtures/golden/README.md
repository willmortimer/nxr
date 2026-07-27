# Golden fixture

Reference flake for **Wave 2.8** automation ergonomics ([ADR-0148](../../docs/adr/0148-automation-ergonomics.md)).
It combines:

- **Apps** with `category` metadata (`backend`, `frontend`, `workspace`)
- **Tasks** in a small DAG (`fmt` → `api-test` / `web-test` → `ci`)
- **Schema v2 contexts** (`backend`, `release`) with shell + environment policy
- Per-task `context` / `shell` refs and a `validation` category on `ci`

## Quick start

From the repository root (requires `nix` on `PATH`):

```bash
# List apps and tasks
cargo run -p nxr-cli -- --flake fixtures/golden list

# Inspect overview (categories)
cargo run -p nxr-cli -- --flake fixtures/golden inspect

# Run the CI gate (serial DAG)
cargo run -p nxr-cli -- --flake fixtures/golden task ci

# Parallel fan-out after fmt
cargo run -p nxr-cli -- --flake fixtures/golden task ci -j 2

# JUnit report from a task run
cargo run -p nxr-cli -- --flake fixtures/golden task ci --junit /tmp/nxr-golden.xml
cat /tmp/nxr-golden.xml

# Evaluate inline nxr metadata
nix eval --json ./fixtures/golden#nxr.aarch64-darwin
```

## What to try next

- `nxr graph ci` / `nxr explain task ci` for DAG introspection
- `nxr inspect task api-test` for context + category on a single node
- `nxr task api-test` to exercise the `backend` execution context
