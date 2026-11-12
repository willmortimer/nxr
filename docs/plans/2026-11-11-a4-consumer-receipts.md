# A4.1 consumer receipts

These rows are real supervisor runs. They are not the MCP unit tests. Phase A4.2 has not been run, so the 22-trial score stays blank. No fleet host was activated. The Lattice and NixPlane flake pins remain `github:willmortimer/nxr/v3.9.0` (`d0e4e759504bc676121a00ca8cd9c6bb7d20bbbe` in Lattice's lock).

The development executable for every row below is `/Users/willmortimer/Developer/nxr/target/debug/nxr`, `nxr-cli 3.9.1`, sha256 `4bd49c21f6a29b06f256b43e00d022545375faf06762436663b7de4c8f5bcbce`. `initialize` reported that path. It is not the v3.9.0 pin.

`nix build --override-input nxr path:/Users/willmortimer/Developer/nxr .#nxr` from Lattice failed while copying the dirty NXR tree (`target/debug/deps/rustcIckHJs` was missing). The runs used the Cargo development binary instead. `path:../nxr` stays the development override until v3.9.1.

Environment identity was not verified. `operations_describe` reported `shell: null` for both tasks. The Lattice log shows Nextest ran the filtered test. That is not a verified toolchain report.

## Gate 1

| Checkout | Bound root | Source revision | Executable |
| --- | --- | --- | --- |
| NXR | `/Users/willmortimer/Developer/nxr` | `2359fce` plus this working tree | development binary above |
| Lattice worktree | `/private/tmp/a4-lattice-pass` | `cc276265cb7f5f5adeb89668f708236d7b84c0fa` | development binary above |
| NixPlane | `/Users/willmortimer/Developer/NixPlane` | `4f1bd660d0b0d64179a740177d5ba9e8cd9d4560` | development binary above |

## Lattice pass

Disposable worktree, policy digest `7662c81d4603e9b80fd8dcfb0913fa37ea48bf9fa0708815ef7888dfb782e4a9`, approved before the start. The only dirty input was the untracked policy file. `operations_describe` reported `execution_allowed: true` and `policy_source: project`.

| Field | Value |
| --- | --- |
| Operation | `lattice:task:rust-test-pkgs` |
| Arguments | `-p`, `lattice-env`, `system_prefers_python3_over_python` |
| Run | `run-1794503801351413000` |
| Status | exited |
| Exit code | 0 |
| Log cursor | 1896 |
| Result | 1 passed, 15 skipped |

## Lattice failure

Second disposable worktree, same policy digest. `crates/lattice-env/src/provider.rs` expected `system:python-broken`. The worktree was deleted after the run.

| Field | Value |
| --- | --- |
| Operation | `lattice:task:rust-test-pkgs` |
| Run | `run-1794503846566964000` |
| Status | exited |
| Exit code | 100 |
| Log cursor | 3090 |
| Result | `system_prefers_python3_over_python` failed: left `system:python3`, right `system:python-broken` |

## NixPlane fmt-check

Policy digest `12c7234b074ab467e50a49e97a9ea0afccae80fa973cc39c63b10690724e026e`. The task is also on the global allowlist, and describe reported `policy_source: allowlist`. The argv was `nxr task -j 8 fmt-check`. It does not name a generation switch, Hub, SYS-5, nixdev, or gmkdev.

| Field | Value |
| --- | --- |
| Operation | `nixplane:task:fmt-check` |
| Run | `run-1794503280142203000` |
| Status | exited |
| Exit code | 1 |
| Log cursor | 36167 |
| Result | `cargo fmt --check` reported existing formatting diffs |

## Recovery

While `run-1794503073244375000` was still `Running`, a new MCP process read that run id and a second start with the same key returned `replayed: true` and did not launch another process. `nxrd` on `/tmp/a4-nxrd.sock` stayed the parent. That run later exited 101 while compiling an unrelated Lattice package. It is not the bounded result above.

## Phase A4.2

| Field | Value |
| --- | --- |
| Trials passed | — |
| Trials required | 21 of 22 |
| Score | — |
| CLI-and-rules baseline | — |
| Unauthorized fleet changes | 0 |
