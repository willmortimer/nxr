# Q-wave contracts

Frozen before implementation. `v3.9.1` stays `e54092c`. No new operation ids, NixPlane pin, or Nextest migration of `apps.test`.

## Identities

Source identity is the committed inputs plus the dirty snapshot. Execution identity is the operation, parameters, inputs, toolchain, platform, and relevant environment. Resource identity is the output path, host, and CPU, memory, and disk reservations.

The same execution identity may reuse a result from another directory. Different execution identities that share one Cargo target conflict.

## Helpers

`resolve_output_resources` returns cargo target and build directories for a cargo command or `--target-dir`. A `nix` command returns no cargo resource. Any other unresolved command returns an error containing `output location is unknown`. A cohort string distinguishes macOS, Linux, architecture, and toolchain. The same canonical path with two cohorts conflicts.

`spawn_guardian` / `guardian_loop` hold an `fs2` lock until the writer process group is dead. Uncertain liveness does not release the lock. A pid file is not proof of ownership. `acquire_ordered` locks paths in canonical order. Direct unmanaged cargo is not claimed as controlled.

`host_budget` reads `NXR_LOCAL_CPU` and `NXR_LOCAL_MEMORY_MIB`. Unset is a finite default, not `u32::MAX`. `reservation_for` uses 1 CPU and 512 MiB when the request is zero. The request does not change the host budget. `disk_preflight` checks the output filesystem. Unknown availability is a refusal. Nothing is deleted.

`child_probe_script` is what the child runs. `parse_child_probe` reads that output. Receipt states are `planned`, `admitted`, `started`, `environment-verified`, `completed`, `artifact-verified`, and `qualified`. A retry writes a new attempt and leaves the old file. Artifact fetch checks the digest and rejects `..` and foreign run ids.

Coalescing attaches only a fully verified identical execution identity. A resource conflict is not an attach. The last observer may terminate the run. An earlier observer only detaches. Idempotency keys stay unchanged.

Passing qualification reuse requires a passed terminal status, exact inputs, toolchain, coverage, and schema. Failed, skipped, not executed, unknown coverage, and a build without tests do not qualify.

Economics fields that were not measured are `unknown`. Measured and estimated savings stay separate.
