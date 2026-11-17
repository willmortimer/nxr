# Q-hardening contracts

Frozen for the parent-only sprint on `8df950379478da91d0d292334f4545a5db29c0e8`. `v3.9.1` stays `e54092c`. No new operation ids, scheduler, or Nextest migration of `apps.test`.

## Guardian

The guardian process acquires the output locks, spawns the writer, and only then publishes the writer pid. The supervisor records that pid and the guardian. It does not hold the flock. A guardian that does not become ready fails admission, and no writer is left running. Cancel signals the writer process group. The guardian drops the locks only when that group is dead.

## Leaf receipts

A pre-spawn probe is predicted. `environment-verified` requires the probe written by the leaf after its environment is applied. One receipt is stored per run, leaf, and attempt. `source_digest` is the admitted operation fingerprint. A leaf observation does not describe another leaf.

## Observers

`coalesce_decision` stays a pure helper. Membership is one SQLite row per `(run_id, observer_id)` updated in an immediate transaction. Detach removes only that observer. The last observer terminates the run. A repeated detach does not remove a different observer.

## Coverage

`managed` is a cargo target or cargo build directory. `none_required` was inspected and needs no NXR exclusive output lock. `unknown` was not inspected. An empty managed list is only `none_required`. `nxr task` at the supervisor stays unknown and does not lock the ambient target. Tasks with `cargoOutput` lock their cargo target through the guardian. Disk reserve is `NXR_DISK_RESERVE_BYTES` or a host-derived floor of 512 MiB capped at 2 GiB. Unknown free space refuses the run.
