# Run observation

Foreground task runs retain an immutable DAG description, authoritative events,
and captured stdout/stderr in the NXR state directory. Compact output, CLI
inspection, and MCP queries use this execution evidence. The read-side
projection is versioned as nxr-run-view-v1; it does not schedule work or
authorize a domain operation.

## CLI

Runner flags precede the task name. Compact is opt-in; existing serial raw and
parallel live defaults remain supported.

~~~sh
nxr --output compact task ci
nxr --output raw task export-manifest > manifest.json

nxr runs inspect RUN_ID
nxr runs diagnostics RUN_ID --node NODE_ID
nxr runs artifacts RUN_ID --node NODE_ID --after 0 --limit 64
nxr runs logs RUN_ID --node NODE_ID --stream stderr --offset 0 --limit 4096
nxr runs logs RUN_ID --node NODE_ID --stream stderr --tail 100
nxr --json runs logs RUN_ID --node NODE_ID --stream stdout --offset 0 --limit 4096
~~~

Inspect, diagnostics, and artifacts return JSON. Log queries write selected
bytes to stdout unless --json requests an envelope. The stream defaults to
stderr; offset defaults to zero and limit to 4096 bytes. Each slice is capped
at 65,536 bytes. Offsets count bytes within one node's selected stream,
including binary and incomplete UTF-8 sequences. JSON uses OutputPayload:
text, plus encoding: "base64" when bytes are not valid UTF-8.

The --tail N option selects lines from at most the latest 64 KiB. It conflicts
with --offset and is available for byte output; JSON callers use offsets.
Artifact queries optionally select a node and accept an exclusive event cursor
in --after. The default page size is 64; the CLI/MCP adapter bounds it to
1–256 records. Use next_cursor for the next page. A full page means another
page may exist; request again to establish the end.

Compact sends progress and failure previews to stderr. Normal verbosity hides
successful child logs. Quiet suppresses successful human output; -v adds
transitions; -vv includes labelled child logs without changing presentation.
Use raw output when the child owns stdout as application data.

## MCP

The existing MCP server exposes foreground DAG evidence through these tools:

| Tool | Arguments | Result |
| --- | --- | --- |
| runs_inspect | run_id | Complete nxr-run-view-v1 projection |
| runs_get_summary | run_id | State, last cursor, causal summary, and terminal-receipt presence |
| runs_get_diagnostics | run_id; optional node | Normalized diagnostics and truncated |
| runs_get_logs | run_id, node; optional stream, offset, limit | Byte-safe payload, next_offset, and eof |
| runs_get_artifacts | run_id; optional node, after, limit | Declaration/verification/failure records, next_cursor, and page_full |

MCP binds a run to the connected workspace's canonical root. A run ID cannot
select another workspace's journal. User CLI inspection can name a run in the
local state directory directly. Existing supervisor-only runs retain their
receipt and log interfaces; the node-scoped log shape applies to foreground
DAG runs. The existing runs_get_receipt tool remains supported.

## Evidence and authority

The frozen plan names the run, operation, workspace, roots, dependencies, and
whether output capture was enabled. Its input_provenance map records node and
parameter names with their origins, never resolved values. Attempts retain their observed source
location and remote attempt reference when available. Journal cursors increase
strictly; corruption or sequence gaps produce an error.

Execution state comes from typed scheduler/supervisor events. Child output
remains bytes: printing a JSON run_completed object cannot complete a task.
A child artifact marker is a declaration; independent byte verification
precedes publication of a verified reference.

The causal summary separates the first known failure, additional failures,
dependent skips, cancellations, timeouts, artifact failures, and explicit
uncertainties. The reported_outcome field retains the authority's terminal
result. State also accounts for completeness: a reported success remains
uncertain while node terminal evidence is outstanding. A later authoritative
node receipt resolves outstanding_nodes without a duplicate run terminal.
A late failure remains failed; authoritative cancellation remains cancelled.
Explicit recorder uncertainty is not cleared by an unrelated node receipt.

Foreground persistence precedes rendering. A critical write failure surfaces
through the execution sink and stops further admission; terminal repainting
is not a persistence acknowledgment. NXI grants, measured hardware authority,
NixPlane enrollment, and domain receipt validation retain their own owners.

## Artifacts and diagnostics

A generic declaration uses the prefix "nxr: artifact " followed by an
[nxr-artifact-ref-v1](../schemas/artifact-ref-v1.schema.json) object.
The existing "lattice: diagnostic-artifact " prefix with schema
lattice.diagnostic.artifact.v1 and kind stargate-recording remains an adapter
into the same declaration model.

~~~json
{
  "schema_version": "nxr-artifact-ref-v1",
  "name": "failure-recording",
  "producer": "stargate",
  "kind": "diagnostic-recording",
  "media_type": "application/json",
  "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "location": {
    "kind": "local-cas",
    "object": "objects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/recording.json"
  },
  "verified": false
}
~~~

A declaration's verified property is discarded. Verification opens a regular
file under the trusted workspace .nxr/artifacts store, refuses symlinks and
unsafe path components, and recomputes SHA256 with a 64 MiB limit. Only the
verifier can construct the proof accepted by the journal publisher. Domain
tools still interpret the recording; NXR does not diagnose Stargate contents.

Markers can span chunks; a final marker without a newline is completed when
its node exits. Artifact pagination uses its own index without scanning logs.
Each node's RunView retains its first 64 unique artifact references and sets
artifacts_truncated when more exist. Use the cursor-indexed artifact API for
the complete history. Projection truncation never suppresses verification
failure summaries or changes a failure into success.
Diagnostics preserve coarse categories and add code, severity, node, source
location, evidence links, and a stable fingerprint. Structured adapters handle
Cargo compiler messages (including warning severity and compiler codes),
Nix messages, libtest/Nextest failures, bounded JUnit failure elements, NXI
error classes, and artifact markers before text fallback. NixPlane transport
and admission refusals retain separate categories. JUnit recognition applies
to queried log bytes; it does not load external XML reports or entities. Identical diagnostics are deduplicated; original bytes remain
queryable.

## Capture, bounds, and compatibility

Evidence lives at runs/RUN_ID/dag/ beneath NXR_STATE_DIR, $XDG_STATE_HOME/nxr,
or NXR's platform state-directory default. SQLite retains plan/event metadata;
separately hashed chunk files retain captured bytes. This wave introduces no
automatic evidence deletion. Explicit --log-dir remains a separate
compatibility tee.

Inherited raw and interactive streams are not intercepted. When logs_captured
is false, log queries explicitly refuse capture instead of fabricating an
empty transcript. Mixed plans list inherited nodes in uncaptured_nodes; their
individual log queries refuse capture even when other nodes were captured.
All-node diagnostics skip inherited nodes and mark coverage incomplete.
Compact retains a bounded 16 KiB preview per node independently of retained
captured logs.

Reader connections are read-only and do not take the writer lock. Replay reads
pages of at most 1024 records up to a fixed observed high-water cursor. Log
reads verify retrieved chunk digests and preserve exact bytes. Bounded
inspection does not rehash unrequested historical log content.

Diagnostic queries inspect at most the latest 64 KiB per stream across 32
nodes and return at most eight diagnostics. Selecting a node narrows the scan.
Physical lines longer than 64 KiB are discarded by the normalizer. Its streaming
parser has a 4 MiB scan budget and Unicode-safe excerpts of at most 240
characters. The truncated and diagnostics_truncated flags report incomplete
query coverage.

Original workflow-leaf run-event-v2 tables remain compatible. Foreground
events use separate tables and cursors in the same journal implementation.
The JSON contracts are
[run-view-v1.schema.json](../schemas/run-view-v1.schema.json) and
[artifact-ref-v1.schema.json](../schemas/artifact-ref-v1.schema.json).
