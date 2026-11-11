# Distributed DAG, after Q2.4b

**Status:** plan. This does not change the Q2.4b remote proof. Q2.4b submits one `native_command` attempt for the Stargate wasm app. It does not schedule `wasm`, `verify`, and `stamp` as three remote nodes.

This follows [2026-11-11-agent-execution.md](2026-11-11-agent-execution.md) and [V4_EXECUTION_PROTOCOL.md](../vision/V4_EXECUTION_PROTOCOL.md).

## Ownership

NXR keeps the run graph and the per-node artifact identities. A reused node stays a local cache decision: the output digest matches the bound inputs, and the bytes are restored before dependents start.

NixPlane keeps one attempt for one native command. The native worker runs an argv. It does not interpret an NXR task graph. The Hub admits, journals, and returns that attempt. It does not choose which DAG node runs next.

## Later placement

When a node needs another platform, the NXR scheduler may place that one node as one `native_command` attempt. The attempt's result is an output digest. Dependents bind to that digest the same way they bind to a local output file. Identical bytes still allow reuse. Changed bytes start the dependent.

One remote attempt remains one node. A graph of three nodes is still one NXR run. Placing all three remotely would be three attempts, each bound by the digest of the previous output, and only when a later design asks for that. Q2.4b does not.

## Out of scope

- Teaching the Hub to schedule task edges.
- Replacing `nxr task` with a remote scheduler.
- A second source-transfer design.
- SYS-5, pin moves, and the live nixdev worker restart.
