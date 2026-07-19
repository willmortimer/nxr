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

Field names use the camelCase vocabulary (`dependsOn`, `workingDirectory`) that
matches [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) and the
Rust `TaskDefinition` type.

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

## Related

- Schema: [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json)
- App authoring: [APP_AUTHORING.md](APP_AUTHORING.md)
- Architecture (task model): [ARCHITECTURE.md](ARCHITECTURE.md) §6
- Fixture: [`fixtures/task-dag/`](../fixtures/task-dag/)
