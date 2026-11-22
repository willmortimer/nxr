# Progress events v1

Frozen for Pleroma Wave 2. `v3.9.2` documents this contract. `v3.9.3` implements it.

Progress is an additive journal event. It does not replace node outcome, exit status, or artifact verification. A missing `total` is indeterminate. Elapsed time never becomes a percentage.

## Event

Journal `event_type` is `progress`. The body is `nxr-progress-v1`:

```json
{
  "schema_version": "nxr-progress-v1",
  "node_id": "rust-tests",
  "stage_id": "nextest",
  "stage_label": "NXR compiler and lint",
  "completed": 187,
  "total": 232,
  "unit": "tests",
  "state": "running"
}
```

`unit` is `steps`, `tests`, `files`, `bytes`, `artifacts`, `samples`, `operations`, or `{ "custom": "name" }`. `state` is `pending`, `running`, `succeeded`, `failed`, `skipped`, or `indeterminate`. `completed` and `total` are absent when unknown. `completed` without `total` stays indeterminate.

RunView reduces these events. Replay of the same journal produces the same stage state. Compact, TUI, JSONL, MCP, and web renderers read that projection. They do not write renderer state back into the log store.

Raw stdout and stderr chunks stay immutable and byte-preserving. Progress messages are not a second copy of those bytes.

Domain adapters may supply stage facts for Cargo, Nextest, Nix, the NXR DAG, NixPlane admission, NXB workers, NXI phases, NXD, CellHost samples, and Stargate. NXR normalizes display fields only. The domain operation state remains authoritative.

Frequent updates coalesce. Memory and query size stay bounded independently of update volume.
