# Task schema v2

**Status:** **partial** — see matrix below. Auto-promotion to `schema_version: 2`
when security/execution fields are used is required as of 2.7.1 ([ADR-0144](adr/0144-auto-schema-v2.md)).

## Shipped / partial / planned matrix

| Area | State | Notes |
|---|---|---|
| Strict parse (`deny_unknown_fields`) | **Shipped** | Document + task + nested v2 objects |
| `inputs` / `outputs` declarations | **Partial** | Parsed and validated; fingerprinting for workspace CAS |
| `cache` policy + workspace result cache | **Experimental 3.1** | Opt-in **local** CAS; `nxr cache explain` ([ADR-0147](adr/0147-two-tier-actions.md)). Secret-bearing tasks disable cache by default (`cache.secretPolicy = "ignore-values"` override). `shared` / `shared-read` fail closed until a transport exists. |
| `resources` scheduling | **Experimental 3.1** | Exclusive locks + soft CPU/memory pools |
| `contexts` module + emit | **Shipped** | `perSystem.nxr.contexts` |
| Auto `schema_version = 2` when v2 fields present | **2.7.1** | Older runners reject instead of ignoring |
| Env-provider secrets (`provider = "env"`) | **Shipped** | Spawn inject; plans show `<runtime>` |
| `provider` file / stdin / sops stubs | **Partial 3.0** | env/file/stdin shipped; Keychain/Vault later |
| `confirm` enforcement | **2.7.1** | TTY / `NXR_ASSUME_YES`; trust DB **3.0** |
| `context.shell` / `task.shell` | **2.7.1** | Via existing `nix develop` wrap path |
| Full env keep/set/unset (inherit) | **Shipped 3.0** | Clean and inherit modes apply keep/set/unset at spawn |
| Project trust / `nxr context` CLI | **Shipped 3.0** | `nxr trust`, `nxr context list\|inspect\|run` |
| Semantic validation (paths, CPU, locks) | **Shipped 3.0** | Rejects invalid v2 metadata at load |
| Remote workspace CAS / workers | **Later** | Local CAS only in 3.1; shared modes tracked in [#2](https://github.com/willmortimer/nxr/issues/2) |

Related ADRs: [0122](adr/README.md), [0144](adr/0144-auto-schema-v2.md),
[0146](adr/0146-secret-provider-ref.md), [0147](adr/0147-two-tier-actions.md),
[0149](adr/0149-context-shell-confirm.md).

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
| `outputs` | Workspace artifacts for opt-in result cache (local CAS when enabled) |
| `cache` | Opt-in result cache policy (`disabled` by default) |
| `resources` | CPU/memory/IO estimates and exclusivity locks |
| `shell` | Optional `devShells.<name>` (shell-only context) |
| `context` | Optional named execution context |
| `parameters` | Typed spawn parameters (`NXR_PARAM_<NAME>`; names only in plans/events) |

Top-level `contexts` maps context names to shell, environment policy, secret
references (logical `ref` strings plus optional `provider`, default `env`), and
`confirm` metadata. Flake-parts consumers emit `schema_version: 2` automatically
when contexts or task `shell` / `context` fields are present.

### Context secrets

`delivery = "env"` secrets with `provider = "env"` (default) resolve from the
caller environment at task spawn (see [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md)).
Non-env providers error at resolve time unless bindings are configured. Contexts
with `confirm = true` prompt before spawn (or require `NXR_ASSUME_YES=1` when
stdin is not a TTY).

```nix
perSystem.nxr.contexts.release = {
  shell = "release";
  secrets.DEPLOY_TOKEN = {
    provider = "env";              # default; logical bindings → 3.0
    ref = "CLOUDFLARE_API_TOKEN";  # env var name when provider = env
    delivery = "env";
  };
  confirm = true;
};

perSystem.nxr.tasks.deploy = {
  app = "deploy";
  context = "release";
};
```

Path-like logical refs require a non-env `provider` and user/HM bindings (3.0).

## Compatibility

- Pure v1 task documents remain `schema_version: 1`.
- Documents with contexts/security fields must be v2 (module auto-promotes).
- Unsupported majors are rejected by runners (never silently ignored).
