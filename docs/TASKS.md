# Declaring and discovering tasks

Tasks are optional, versioned orchestration metadata. They coordinate flake apps;
they do not replace them. There is no side-channel `nxr.toml` / YAML task file —
authors declare tasks in the flake, and `nxr` loads them by evaluating a
documented flake attribute.

## Author declaration (`perSystem.nxr.tasks`)

Import `nxr.flakeModules.default` and declare tasks next to apps:

```nix
imports = [ nxr.flakeModules.default ];

perSystem = { ... }: {
  nxr.apps.fmt = {
    description = "Format sources";
    script = ''
      echo fmt
    '';
  };

  nxr.tasks = {
    fmt = {
      description = "Format sources";
      app = "fmt";
    };

    test = {
      description = "Run tests";
      dependsOn = [ "fmt" ];
      app = "test";
    };

    ci = {
      description = "CI gate";
      dependsOn = [ "test" ];
      app = "ci";
      category = "validation";
      aliases = [ "check" ];
    };
  };
};
```

### Task fields (MVP)

| Field | Required | Notes |
|---|---|---|
| `app` | yes | Flake app leaf name (`apps.<system>.<name>`) |
| `description` | no | Short description for listings |
| `dependsOn` | no | List of task names; default `[]` |
| `workingDirectory` | no | Policy or path (for example `flake-root`) |
| `hidden` | no | Omit from default listings; default `false` |
| `category` | no | Logical grouping for listings |
| `aliases` | no | Alternate names for explicit task commands (see below); default `[]` |

Field names use the camelCase vocabulary (`dependsOn`, `workingDirectory`) that
matches [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) and the
Rust `TaskDefinition` type.

### Name resolution

| Invocation | Resolves aliases? |
|---|---|
| `nxr <name>` (bare app run) | **No** — apps only |
| `nxr task`, `nxr graph`, `nxr inspect task`, `nxr watch` | Yes |
| `nxr plan <name>` | Yes when no app matches (apps win first) |

Aliases map to a single canonical task name. Ambiguous aliases (claimed by more
than one task) are rejected.

## Evaluable flake attribute

The module emits a **versioned task document** at:

```text
nxr.<system>
```

Shape (JSON via `nix eval --json`):

```json
{
  "schema_version": 1,
  "tasks": {
    "fmt": { "app": "fmt", "description": "Format sources", "dependsOn": [], "hidden": false },
    "test": { "app": "test", "dependsOn": ["fmt"], "hidden": false },
    "ci": { "app": "ci", "dependsOn": ["test"], "category": "validation", "hidden": false }
  }
}
```

Smoke check against the fixture flake:

```bash
nix eval --json ./fixtures/task-dag#nxr.aarch64-darwin
nix eval --json ./fixtures/task-dag#nxr.x86_64-linux
```

Replace the system string with `builtins.currentSystem` as needed.

### Discovery rules

| Evaluation result | Behavior |
|---|---|
| Attribute missing (flake has no `nxr` output) | Empty task set (OK) |
| Document with `tasks = { }` | Empty task set (OK) |
| Unsupported `schema_version` major | Typed schema error |
| Other Nix evaluation failures | Mapped like other Nix adapter errors |

## Argument forwarding (V2 freeze)

Trailing CLI arguments after the task name are forwarded to the **root task’s
app only**. Every dependency node receives an empty argument list.

```bash
nxr task ci -- --flag
# fmt → test → ci; only the `ci` app sees `--flag`
```

This is the frozen V2 policy (`argument_forwarding: "root"` on the execution
plan envelope). Richer per-node forwarding is deferred; there is no interactive
`--stdin <task>` picker.

## Stdin ownership

| Mode | Stdin |
|---|---|
| Serial interactive (`-j 1`, no `--output` / `--events`) | Inherited by the running child |
| Parallel (`-j > 1`) or multiplex (`--output` / `--events`) | Null/closed for **all** supervised children |

Parallel and labeled/events paths must not inherit caller stdin into multiple
children — ownership is deterministic (closed).

## Schema freeze (V2.0)

The task document (`schema_version: 1`), execution-plan envelope, and
execution-event vocabulary are **frozen** for the V2.0 release:

| Artifact | Location | Notes |
|---|---|---|
| Task document | [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) | Emitted at `nxr.<system>`; unsupported majors rejected |
| Execution plan | [`schemas/execution-plan-v1.schema.json`](../schemas/execution-plan-v1.schema.json) | Built for `nxr plan <task>` and the task scheduler |
| Events | Rust `Event` in `nxr-task` | JSON schema file deferred to Phase 16; enum is authoritative until then |

Additive optional fields may appear within major version 1. Breaking changes
require a new major `schema_version`. See [COMPATIBILITY.md](COMPATIBILITY.md).

## Related

- Schema: [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json)
- App authoring: [APP_AUTHORING.md](APP_AUTHORING.md)
- Architecture (task model): [ARCHITECTURE.md](ARCHITECTURE.md) §6
- Fixture: [`fixtures/task-dag/`](../fixtures/task-dag/)
