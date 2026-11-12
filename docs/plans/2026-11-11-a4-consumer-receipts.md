# A4.1 consumer receipts

These rows are real supervisor runs. They are not the MCP unit tests. Phase A4.2 has not been run, so the 22-trial score stays blank. No fleet host was activated. The Lattice and NixPlane flake pins remain `github:willmortimer/nxr/v3.9.0` (`d0e4e759504bc676121a00ca8cd9c6bb7d20bbbe` in Lattice's lock).

The primary candidate is `db8961a5f3b96e3e3ea41c514f5f233934951b15`. The `8d65888` build and the Cargo binary are history.

## Candidate

`db8961a5f3b96e3e3ea41c514f5f233934951b15` changes only the Nix package version from 3.9.0 to 3.9.1. `nix build --override-input nxr github:willmortimer/nxr/db8961a5f3b96e3e3ea41c514f5f233934951b15 .#nxr` from Lattice produced `/nix/store/yd2gs1bagfj3sa3rn102fq8g40flji4z-nxr-3.9.1`. The binary is `bin/nxr`, `nxr-cli 3.9.1`, sha256 `7b172b7911f23428db74080c0bc5e184d0abd736453c53843886e28dbaf706ae`. This is not `7a5a0204…`.

`initialize` in NXR, Lattice, and NixPlane reported that store path. Lattice and NixPlane MCP configs override `nxr` to that commit. The flake pins remain v3.9.0. This commit is the frozen A4.2 candidate. The 22-trial score stays blank.

## Store-binary smoke

The daemon was `/nix/store/yd2gs1bagfj3sa3rn102fq8g40flji4z-nxr-3.9.1/bin/nxr daemon start`, socket `/tmp/a4-nxrd-3191.sock`. `shasum` matched `7b172b7911f23428db74080c0bc5e184d0abd736453c53843886e28dbaf706ae`.

NixPlane `nixplane:task:fmt-check` started as `run-1794512774339171000` with workspace root `/Users/willmortimer/Developer/NixPlane`. The command was that store binary running `task -j 8 fmt-check`. It exited 1. The log cursor is 35968. The log is existing formatting diffs, not a fleet activation.

Lattice `lattice:task:rust-test-pkgs` did not start. The live checkout `/Users/willmortimer/Developer/lattice-ecosystem/lattice` is trusted and has no approved policy digest, so the store binary returned `policy approval required`. No Lattice process was launched. The earlier Lattice passes remain the Cargo-binary rows below.

## Previous immutable candidate

`nix build --override-input nxr github:willmortimer/nxr/8d65888a523be3bfd0812437df8faa070c77833a .#nxr` from Lattice produced `/nix/store/mv161ycs05gygf8m3hjaxgsd8nvmr5jg-nxr-3.9.0`. The binary is `bin/nxr`, `nxr-cli 3.9.1`, sha256 `7a5a0204d9a11a8e0b142a583009de9376826766242687d51868fb2cdc8c7a04`. This is not `4bd49c21…`. The package name was still 3.9.0.

## Historical Cargo binary

The development executable for the operation rows below is `/Users/willmortimer/Developer/nxr/target/debug/nxr`, `nxr-cli 3.9.1`, sha256 `4bd49c21f6a29b06f256b43e00d022545375faf06762436663b7de4c8f5bcbce`. `initialize` reported that path. It is not the v3.9.0 pin.

`nix build --override-input nxr path:/Users/willmortimer/Developer/nxr .#nxr` from Lattice failed while copying the dirty NXR tree (`target/debug/deps/rustcIckHJs` was missing). Those runs used the Cargo development binary.

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
