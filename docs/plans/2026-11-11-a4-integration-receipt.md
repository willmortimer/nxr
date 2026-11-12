# A4.1 integration receipt

Phase A4.2 has not been run. The 22-trial score stays blank. The seven, then nine, MCP unit tests are not this receipt. No fleet host was activated. The Lattice and NixPlane flake pins remain `github:willmortimer/nxr/v3.9.0`.

## Executable identity

Cold `initialize` was sent to the development binary, not to a `nix run` of the v3.9.0 pin.

| Field | Value |
| --- | --- |
| Command | `/Users/willmortimer/Developer/nxr/target/debug/nxr mcp` |
| Reported executable | `/Users/willmortimer/Developer/nxr/target/debug/nxr` |
| `nxr --version` | nxr-cli 3.9.1 |
| sha256 | `9636203c42538ffc70ff577b95542f95ce49b2846efc2284b7d5726024bbb416` |
| NXR checkout | `825e4ad` plus the uncommitted qualification changes |
| Protocol | `2024-11-05` |

Cursor configs in Lattice and NixPlane override `nxr` with `path:../nxr` so they build this checkout. There is no pushed revision of this sprint to put in a `github:` URL yet.

## Checkouts

| Checkout | Revision | MCP config | Policy |
| --- | --- | --- | --- |
| NXR | `825e4ad` plus this working tree | `.cursor/mcp.json` runs `nix run .#nxr -- mcp` | allowlist only |
| Lattice | `a89c84f3` | `.cursor/mcp.json` overrides `nxr` to `path:../nxr` | `lattice:task:rust-test-pkgs`, `lattice:task:cli-build` |
| NixPlane | `47f4e35` | `.cursor/mcp.json` overrides `nxr` to `path:../nxr` | `nixplane:task:test`, `fmt-check`, `lint`, `deny` |

NixPlane also has `.nxr/workspace-id` set to `nixplane`. The git remote name is `NixPlane`, and without that file the catalog ids would not match the policy.

## Operations started

No real configured task was started. `rust-test-pkgs`, `cli-build`, and the NixPlane checks were not launched, so this receipt has no run id, terminal status, or log cursor for them.

Not recorded here, because they used fixture commands:

- accepted and rejected parameters
- a dirty directory conflict
- a log offset
- a restart onto the same supervisor run
- a duplicate key
- two checkouts sharing one idempotency key
- a refused cancel of a live process

## Phase A4.2

| Field | Value |
| --- | --- |
| Trials passed | — |
| Trials required | 21 of 22 |
| Score | — |
| CLI-and-rules baseline | — |
| Time to first diagnostic | — |
| Extra compiler launches | — |
| Manual interventions | — |
| Unauthorized fleet changes | 0 |
| Failure class | — |
