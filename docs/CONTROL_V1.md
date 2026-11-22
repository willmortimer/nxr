# Child control channel v1

Frozen for Pleroma Wave 2. `v3.9.2` documents this contract and does not implement it.

A child opts in by writing `nxr-control-v1` messages. The channel is an inherited descriptor or authenticated Unix socket on Unix, an inherited or named pipe on Windows, and bounded frames on a remote attempt. Each message is bound to the run and node that spawned the child.

## Messages

`stage_start`, `stage_update`, and `stage_complete` carry the same fields as a progress event, plus `kind`. `diagnostic` carries severity, code, and message. `artifact_declaration` carries an unverified artifact reference. Verification still uses the existing artifact lookup.

```json
{
  "schema_version": "nxr-control-v1",
  "kind": "stage_update",
  "node_id": "rust-tests",
  "stage_id": "nextest",
  "stage_label": "NXR compiler and lint",
  "completed": 187,
  "total": 232,
  "unit": "tests",
  "state": "running"
}
```

A child may report counts. It cannot mark the parent task successful, skip exit-status checks, or satisfy a required artifact digest. Unknown kinds are ignored and recorded as diagnostics. The channel never carries secret values.
