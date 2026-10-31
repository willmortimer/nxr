# ADR-0175: Session runs are owned by `nxrd`

- **Status:** Accepted
- **Date:** 2026-10-28
- **Target release:** 3.9.0 (does not move `v3.8.0`)
- **Related ADRs:** ADR-0157, ADR-0164

## Context

`nxr session start` recorded a process and waited for it on a thread inside the
CLI. When that CLI exited, the exit code and logs were gone. ADR-0157 keeps
`nxrd` optional for cache methods and for `nix run`. A session that must stay
observable needs a parent that outlives the CLI, without making ordinary Nix
execution depend on the daemon.

## Decision

1. The existing per-user `nxrd` socket gains an execution role for `nxr session`
   only. Cache, plan, and log methods stay optional. `nix run` does not connect
   for this and still works when the daemon is absent or `NXR_DAEMON=off`.
2. `nxr session start` submits the run and fails closed when the supervisor is
   down. The CLI does not spawn or wait on the child.
3. The supervisor is the parent. It stores the pid and start time, writes the
   child's stdout and stderr, records the exit code after the CLI has gone, and
   releases the budget when the run is no longer `running`.
4. Admission takes one exclusive lock for the budget check and the reservation,
   so two starts cannot both pass.
5. Restart reconciles pid and start time. A live pid whose start time does not
   match is not the old run and is recorded as `lost`, without inventing an
   exit code.
6. Cancel of a remote submit calls `cancel_execution` with the stored attempt
   id before the local process group is signalled. Killing only the local
   client is not success.
7. Unix process groups are the ownership proof in this pass. cgroups, launchd,
   and Job Objects are not.

## Consequences

`nxr daemon` must be running for `nxr session`. It must not be required for
`nix run`. This ADR does not turn `nxrd` into a second DAG scheduler and does
not move the `v3.8.0` tag.
