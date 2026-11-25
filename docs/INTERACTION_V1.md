# Resumable input sessions

NXR uses `nxr-interaction-v1` for typed requests shared by terminal and headless
frontends. The session owns input collection. The existing supervisor owns any
process it eventually launches; NXI, NXD, and other domain systems still own
authorization and evidence verification.

## CLI and MCP

Use the operation ID returned by `nxr op search`. Keep the same workspace
selection (`--flake` when supplied) for input collection and all interaction
commands. Canonical local `path:` references retain their checkout identity
across resume, even when the caller is in another directory:

```sh
nxr --json op inputs <operation-id>
nxr --json op resolve --inputs values.json <operation-id>
nxr --json interaction inspect <session-id>
nxr --json interaction answer --request <request-id> --revision 1 \
  --value '"selected-public-value"' <session-id>
nxr interaction prompt <session-id>
nxr --json interaction inbox
nxr --json interaction inbox --resume <session-id>
nxr --json interaction resume --revision 2 <session-id>
```

`answer` also accepts `--value-file` containing one JSON value and an optional
`--observation-digest` matching the outstanding request. Raw credentials are
never valid answers to `secret_ref`; provide an authorized logical reference.
A caller-supplied `verified` flag cannot make facts, artifacts, or grants trusted.

`interaction prompt` walks outstanding typed requests on a TTY, offers
review/edit before execute, and leaves every session intact when one prompt is
cancelled. Non-TTY and CI callers keep structured `answer` / `--set` /
`--inputs` paths and never receive a prompt.

`interaction inbox` lists durable sessions (`session_id`, `operation_id`,
`revision`, waiting/executing/cancelled/uncertain status). `--resume` reconnects
through the existing single-use lease path so an already-acknowledged launch is
never started twice.

The existing MCP server adds `operations_inputs`, `operations_resolve`,
`interactions_get`, `interactions_answer`, `interactions_resume`, and
`interactions_cancel`. Tool arguments cannot select a workspace or caller
identity. The stdio adapter binds the current local user and launched workspace.
`operations_resolve.inputs` is a map from node ID to typed parameter values, or
the complete `nxr-input-values-v1` envelope. Existing string `parameters` become
CLI-style overrides. An optional `idempotency_key` returns the same resolution
for identical source and inputs, refusing conflicts.

Missing typed inputs on the production `operations_start` path return
`input_required` immediately. Fully resolved typed tasks use `--inputs` before
the selected task name; they are not converted into positional leaf arguments.
Existing operation admission remains authoritative. New input collection does
not make a non-executable operation executable.

## Durable boundaries

The private per-workspace state directory stores session metadata, accepted
public values/reference envelopes, and a frozen invocation descriptor. Unix
directories use mode 0700 and files use 0600. Symlink records and nonregular
sidecars are refused; reads are bounded and updates are atomic.

The descriptor binds source identity, task declarations and DAG, NXR executable,
system, invocation directory, context, and relevant safe task flags. Git source
identity includes existing dirty-tree and lockfile machinery. A standalone
non-Git fixture instead receives a bounded complete-tree identity. Source,
declaration, runner, or system changes require a new resolution before launch.
The original invocation directory is retained across reconnects; process-wide
`chdir` is not used for MCP input validation.

Answers bind session, request, revision, local authority, declared type, and
observation digest where applicable. Identical retries are idempotent after the
session revision advances. Conflicting and stale replies fail. The default
pre-execution lifetime is 24 hours; expiry and cancellation cannot start work.

Before resuming, NXR rechecks project trust and the approved operation/parameter
policy, reloads current source and task metadata, and validates all final typed
inputs and local path constraints. Domain references still require their
owning system to verify current authority and freshness. A prompt answer is
never a signed installation grant.

A durable execution reservation and predetermined supervisor run ID are stored
before submission. Only the first reservation returns launch permission. A
lost acknowledgement produces `uncertain`; reconnects inspect the existing run
and never automatically launch a replacement. Running cancellation is recorded
as complete only after supervisor confirmation.

## Terminal and sensitive invocation limits

Existing Dialoguer widgets implement `InteractionFrontend` with required versus
default labeling, boolean/choice (and multichoice) widgets, and validation
retry. Non-TTY execution never waits for stdin. NXR-owned prompts run before
child execution. `nxr select` / `--select` lists apps and tasks with
descriptions when stdin and stdout are TTYs; substring `--filter` is optional.

Durable collection refuses arbitrary forwarded leaf arguments, raw environment
`--set-env` values, and arbitrary Nix option payloads because they have no typed
sensitivity contract. Declare public inputs or secret references instead.
Fully supplied foreground execution keeps its existing behavior. Child-owned
TTY applications and raw/TUI terminal ownership stay in the foreground; a
future qualified PTY bridge is separate work.

See [typed task schema v3](TASK_SCHEMA_V3.md) and the
[interaction schema](../schemas/interaction-v1.schema.json).
