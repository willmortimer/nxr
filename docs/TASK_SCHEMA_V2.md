# Task schema v2 (draft)

**Status:** draft specification only — **not implemented** in current nxr releases.

Current runners accept only [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json)
(`schema_version: 1`). A document with `schema_version: 2` is **rejected** by v1 runners
(unsupported major). Do not emit v2 from flake modules until an nxr release advertises
support.

This draft captures execution-affecting fields that must not be added to schema v1.
Schema v1 tolerates unknown task fields; older runners would silently ignore security
and execution metadata. Schema v2 uses a **strict** task envelope (`additionalProperties:
false`) so unknown execution-affecting fields fail validation instead of being dropped.

Related decisions:

| ADR | Topic | Roadmap |
|---|---|---|
| [ADR-0135](adr/README.md) | Opt-in task result caching for declared workspace outputs | Runtime: later (post-3.0) |
| [ADR-0138](adr/README.md) | Task resource declarations and cooperative job control | Schema: 3.0 |

Execution **contexts**, secret delivery, dependency states (`database@ready`), process
nodes, and confirmation requirements are planned in the same major version family but
documented separately in [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md). They are **not**
defined in this draft schema file.

Internal design notes (gitignored packet): `docs/internal/nxr-next-improvements/`.

## Artifact

| Item | Location |
|---|---|
| JSON Schema (draft) | [`schemas/task-v2.schema.json`](../schemas/task-v2.schema.json) |
| Frozen v1 schema | [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) |
| Authoring guide (v1) | [TASKS.md](TASKS.md) |

## Envelope

Same evaluable flake attribute as v1: `nxr.<system>`.

```json
{
  "schema_version": 2,
  "tasks": { },
  "apps": { },
  "discoveryInputs": []
}
```

All v1 task fields remain available. New per-task fields:

| Field | Purpose |
|---|---|
| `inputs` | Declared paths, environment, Git state, and upstream output bindings for cache keys and structured wiring |
| `outputs` | Repository-relative workspace artifacts that may be restored from a result cache |
| `cache` | Opt-in result cache policy (`disabled` by default) |
| `resources` | CPU/memory/IO estimates and named exclusivity locks |

## Example (illustrative — not runnable today)

```nix
perSystem.nxr.tasks.test = {
  app = "test";

  inputs = {
    paths = [
      "Cargo.toml"
      "Cargo.lock"
      "crates/**"
      "tests/**"
    ];
    env = [
      "RUSTFLAGS"
      {
        name = "NEXTEST_PROFILE";
        required = false;
        secret = false;
      }
    ];
    includeGitState = false;
  };

  outputs = [
    {
      path = "target/nextest/default";
      mode = "replace";
      optional = true;
    }
    {
      path = "reports/junit.xml";
      mode = "report";
      optional = true;
    }
  ];

  cache = {
    mode = "local";
    version = "1";
    restore = true;
    save = true;
    failures = false;
  };

  resources = {
    cpu = 2;
    memory = "4GiB";
    io = "heavy";
    network = false;
    exclusive = [ "cargo-target" ];
  };
};
```

## `inputs`

Fingerprint sources for task result caching (ADR-0135). Only declared inputs
participate — the full inherited environment is never hashed.

| Subfield | Type | Notes |
|---|---|---|
| `paths` | `[string]` | Repository-relative paths and globs |
| `env` | `[string \| object]` | Variable names or `{ name, required?, secret? }` bindings |
| `includeGitState` | `bool` | Include Git tree/commit state in the cache key |
| `bindings` | `{ name → { from } }` | Named wiring to upstream task outputs (`task.output`) |

Secret env bindings disable caching by default. Secret **values** never appear in
plans, events, or cache metadata.

## `outputs`

Declared workspace artifacts under the flake root.

| Field | Required | Notes |
|---|---|---|
| `path` | yes | Repository-relative path |
| `mode` | no | `replace` (default), `merge`, `verify-only`, `report` — initial runtime may implement only `replace` and `report` |
| `optional` | no | Missing output at save time does not fail the task |

Store paths produced by Nix derivations are owned by Nix, not copied into the nxr
result cache.

## `cache`

Opt-in task result cache policy. Default `mode` is `disabled`.

| Field | Notes |
|---|---|
| `mode` | `disabled`, `local`, `shared-read`, `shared` |
| `version` | Author-controlled salt for behavior changes not captured by inputs |
| `restore` | Restore declared outputs on cache hit |
| `save` | Persist successful results |
| `failures` | Cache failed runs (disabled in initial implementations) |

Runtime caching is **out of scope** for this draft. See ADR-0135 and the internal
[task result cache](internal/nxr-next-improvements/nxr-next/02-task-result-cache.md)
note for execution flow, CAS layout, and CLI surface.

## `resources`

Cooperative scheduling hints (ADR-0138). Global `-j` does not model memory, I/O, or
shared-workspace conflicts.

| Field | Notes |
|---|---|
| `cpu` | Scheduler token demand (not a hard OS quota) |
| `memory` | Estimated peak (`4GiB`, `512MiB`, …) |
| `io` | `light`, `normal` (default), `heavy` |
| `network` | Expected network use (policy metadata) |
| `exclusive` | Named mutexes (e.g. `cargo-target`, `pnpm-workspace-install`) |

## Validation and compatibility

| Runner | `schema_version: 1` | `schema_version: 2` |
|---|---|---|
| nxr ≤ 2.x (current) | Accepted | **Rejected** (unsupported major) |
| Future v2 runner | Rejected when strict | Accepted; unknown task fields **rejected** |

`crates/nxr-task` still exports `SCHEMA_VERSION = 1`. This draft does not change Rust
types, Nix module emission, or flake evaluation.

## Delivery stages (planned)

1. Draft schema and documentation (this change).
2. Rust structs, strict validation, and flake module emission behind a feature gate.
3. Resource-aware scheduler (ADR-0138, roadmap 3.0).
4. Declared inputs/outputs and cache explain (`nxr explain`, `nxr plan --json`).
5. Local result cache runtime (ADR-0135, post-3.0).

## Related

- [TASKS.md](TASKS.md) — v1 authoring and discovery
- [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) — contexts, secrets, structured I/O narrative
- [COMPATIBILITY.md](COMPATIBILITY.md) — supported schema majors per release
- [ROADMAP.md](ROADMAP.md) — 3.0 task schema v2 milestone
