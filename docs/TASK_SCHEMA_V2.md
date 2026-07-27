# Task schema v2 (draft)

**Status:** **partial** — strict parse and Rust types for `schema_version: 2` are
implemented in `crates/nxr-task`, including named **contexts** (H2). Secret
resolution, runtime context application, result-cache runtime, and resource
scheduling runtime remain later work (H3 and roadmap 3.0).

Runners accept [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json)
(`schema_version: 1`) and [`schemas/task-v2.schema.json`](../schemas/task-v2.schema.json)
(`schema_version: 2`) at parse time. Schema v1 tolerates unknown task fields; schema v2
rejects unknown document and task fields. Do not emit v2 from flake modules until an nxr
release advertises full v2 runtime support beyond parse/validation.

This draft captures execution-affecting fields that must not be added to schema v1.
Schema v1 tolerates unknown task fields; older runners would silently ignore security
and execution metadata. Schema v2 uses a **strict** task envelope (`additionalProperties:
false`) so unknown execution-affecting fields fail validation instead of being dropped.

Related decisions:

| ADR | Topic | Roadmap |
|---|---|---|
| [ADR-0135](adr/README.md) | Opt-in task result caching for declared workspace outputs | Runtime: later (post-3.0) |
| [ADR-0138](adr/README.md) | Task resource declarations and cooperative job control | Schema: 3.0 |

Execution **contexts** (named shell/env/secret-ref bundles), secret delivery,
dependency states (`database@ready`), process nodes, and confirmation
requirements are documented in [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).
Context metadata is defined in this schema; runtime application and secret
resolution are not yet implemented.

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
  "discoveryInputs": [],
  "contexts": { }
}
```

All v1 task fields remain available. New per-task fields:

| Field | Purpose |
|---|---|
| `inputs` | Declared paths, environment, Git state, and upstream output bindings for cache keys and structured wiring |
| `outputs` | Repository-relative workspace artifacts that may be restored from a result cache |
| `cache` | Opt-in result cache policy (`disabled` by default) |
| `resources` | CPU/memory/IO estimates and named exclusivity locks |
| `shell` | Optional devShell name for a shell-only execution context |
| `context` | Optional named execution context reference |

Top-level `contexts` maps context names to shell, environment policy, secret
references (logical `ref` strings only), and `confirm` metadata.

## Example contexts (runtime: env-provider secrets partial — H3)

`delivery = "env"` secrets resolve from the caller environment at task spawn
(see [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md)). `file` / `stdin` and sops/HM
bindings are not implemented yet.

```nix
perSystem.nxr.contexts = {
  backend = {
    shell = "backend";
    environment = {
      mode = "inherit";
      set.RUST_LOG = "debug";
    };
  };

  release = {
    shell = "release";
    environment = {
      mode = "clean";
      keep = [ "HOME" "SSH_AUTH_SOCK" ];
      set.RELEASE_CHANNEL = "stable";
    };
    secrets.DEPLOY_TOKEN = {
      ref = "fixture/prod/deploy-token";
      delivery = "env";
    };
    confirm = true;
  };
};

perSystem.nxr.tasks.deploy = {
  app = "deploy";
  context = "release";
};
```

See [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) for the full design. Flake-parts
module options live under `perSystem.nxr.contexts` (`nix/modules/contexts.nix`).

## Example task cache/resources (illustrative — not runnable today)

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
| nxr ≤ 2.x (pre-H1) | Accepted | **Rejected** (unsupported major) |
| nxr (H1+) | Accepted; unknown task fields tolerated | Accepted at parse; unknown fields **rejected**; contexts parse/validate (H2); secret/runtime still later |

`crates/nxr-task` exports `SCHEMA_VERSION = 1` (default for new documents) and
`SCHEMA_VERSION_V2 = 2` with strict parse via `parse_task_document`. Nix module emission
and flake evaluation still emit v1 until a later milestone enables v2 authoring.

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
