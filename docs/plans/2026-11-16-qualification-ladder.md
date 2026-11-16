# Qualification ladder

**Status:** design. Do not build the selector or retarget Cargo in this document's commit. v3.9.1 stays the signed baseline on `e54092c`. Cursor quota is not a release-tag dependency.

## Levels

L0 runs on every relevant code change. It is deterministic: cached unit tests and the focused protocol checks. It does not call a model.

L1 runs when the change touches tool discovery, MCP behavior, execution policy, or environment reporting. It is one to four targeted model trials with a hard budget. A trial that makes no tool call is `not_executed`, not a pass or a fail.

L2 is the full adoption corpus. It is scheduled and budgeted. It is not required to sign a patch release whose L0 evidence and an approved exception already cover the change.

## Evidence manifest

One manifest links:

- source revision
- executable digest
- CI receipts, including the host and Linux gate logs
- model receipts, each named by the executable they actually ran
- known failures
- approved exceptions

The v3.9.1 exception is the example. Historical Cursor MCP is 21/22 on `ba911c2` / sha256 `b34ef0a4`. The shipped executable is sha256 `88a6722c` at `e54092c`, with host and Linux gates of 1224 passes and the five protocol checks recorded in [2026-11-11-nxr-3.9.1-release.md](2026-11-11-nxr-3.9.1-release.md). Cursor smoke session `a89dcc86-cbef-46dd-8549-8c749a13f10d` is `not_executed_quota_exhausted`. That score is not a 21/22 measurement of `e54092c`.

A later dependency patch asks the manifest for the regressions that dependency affects. It does not default to 22 Cursor sessions.

## Build isolation

Host and Linux `CARGO_TARGET_DIR` values stay separate. A disk preflight refuses a shared target before a compile starts. One writer owns an expensive target directory. The v3.9.1 host-gate failure was a Linux ELF executed from a shared target, followed by a full target tree that filled the disk. Deleting another project's cache is not a remedy.

## Known flake

`quality (macos-latest, nix latest)` failed once on `v3.9.1` in `nxr-cli::cli watch_metadata_restart_rediscovers_apps`. Stdout closed after one `hello from basic-apps`. The same test passed on the local host gate and on the Nix 2.18 macOS job. The rerun passed. Keep the test. Do not delete or weaken it. Treat the single failure as a flake candidate until a second failure shows a source defect.

NixPlane and Lattice formatting diffs stay in those repositories.

## Caller identity

Cursor MCP does not prove that a tool call came from the parent rather than a worker. A client-supplied `caller_role` or `authorized` flag is not authority, and a token stored where the same Unix user can read it is not a substitute. Local execution stays the trusted workspace, the project policy, and the local allowlist. `operations_plan_batch` leaves `authorized` false and `deploy_allowed` false. The worktree shell guard is a separate best-effort check. It does not grant fleet or Hub authority.
