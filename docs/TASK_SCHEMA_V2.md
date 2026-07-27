# Task schema v2

**Status:** **partial** — see matrix below. Auto-promotion to `schema_version: 2`
when security/execution fields are used is required as of 2.7.1 ([ADR-0144](adr/0144-auto-schema-v2.md)).

## Shipped / partial / planned matrix

| Area | State | Notes |
|---|---|---|
| Strict parse (`deny_unknown_fields`) | **Shipped** | Document + task + nested v2 objects |
| `inputs` / `outputs` / `cache` / `resources` types | **Parse-only** | Runtime cache/scheduling → 3.1 |
| `contexts` module + emit | **Shipped** | `perSystem.nxr.contexts` |
| Auto `schema_version = 2` when v2 fields present | **2.7.1** | Older runners reject instead of ignoring |
| Env-provider secrets (`provider = "env"`) | **Partial** | Spawn inject; plans show `<runtime>` |
| `provider` ≠ env / file / stdin delivery | **Planned 3.0** | Hard-error until implemented |
| `confirm` enforcement | **2.7.1** | TTY / `NXR_ASSUME_YES`; trust DB → 3.0 |
| `context.shell` / `task.shell` | **2.7.1** | Via existing `nix develop` wrap path |
| Full env keep/set/unset (inherit) | **Partial → 3.0** | Clean mode stronger; inherit unset → 3.0 |
| Project trust / `nxr context` CLI | **Planned 3.0** | |
| Semantic validation (globs, CPU, locks) | **Planned 3.0** | |
| Result cache + resource scheduler | **Planned 3.1** | [ADR-0147](adr/0147-two-tier-actions.md) |

Related ADRs: [0122](adr/README.md), [0144](adr/0144-auto-schema-v2.md),
[0146](adr/0146-secret-provider-ref.md), [0149](adr/0149-context-shell-confirm.md).

Schema v1 tolerates unknown task fields; **do not** emit security fields under
v1. Schema v2 rejects unknown execution-affecting fields.

## Artifact

| Item | Location |
|---|---|
| JSON Schema | [`schemas/task-v2.schema.json`](../schemas/task-v2.schema.json) |
| Frozen v1 schema | [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) |
| Authoring guide | [TASKS.md](TASKS.md) |
| Execution contexts | [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) |

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

### Per-task fields (v2 additions)

| Field | Purpose |
|---|---|
| `inputs` | Declared paths, environment, Git state, upstream bindings |
| `outputs` | Workspace artifacts for future result cache |
| `cache` | Opt-in result cache policy (`disabled` by default) |
| `resources` | CPU/memory/IO estimates and exclusivity locks |
| `shell` | Optional `devShells.<name>` (shell-only context) |
| `context` | Optional named execution context |

### Context secrets

```nix
secrets.DEPLOY_TOKEN = {
  provider = "env";           # default; logical bindings → 3.0
  ref = "CLOUDFLARE_API_TOKEN"; # env var name when provider = env
  delivery = "env";
};
```

Path-like logical refs require a non-env `provider` and user/HM bindings (3.0).

## Compatibility

- Pure v1 task documents remain `schema_version: 1`.
- Documents with contexts/security fields must be v2 (module auto-promotes).
- Unsupported majors are rejected by runners (never silently ignored).
