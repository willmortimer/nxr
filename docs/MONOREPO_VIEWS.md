# Namespaced monorepo views

Large flakes often expose many apps and tasks. `nxr` can filter `list` /
`inspect` by **category** (flake metadata) and optional **namespace**
(project view file). Neither feature becomes an operation authority:
leaf commands remain ordinary `apps.<system>.<name>` outputs.

## Authority boundary

| Layer | Role |
|---|---|
| `apps.<system>.*` | Canonical runnable operations (`nix run` escape hatch) |
| `nxr.<system>` tasks + optional `apps` listing metadata | Orchestration + list categories |
| `nxr.projects.json` (optional) | Non-authoritative namespace membership for views |

Missing `nxr.projects.json` is fine. Apps remain runnable with `nxr <app>` /
`nix run .#<app>` without any project file. Do **not** treat the projects
file as a second task DSL.

## Categories (flake metadata)

### Tasks

Set `category` on `perSystem.nxr.tasks.<name>` (see [TASKS.md](TASKS.md)).

### Apps

Set `category` on `perSystem.nxr.apps.<name>`. The flake-parts module emits:

1. `meta.category` on the standard flake app
2. listing metadata under `nxr.<system>.apps.<name>.category`

`nxr` merges that listing metadata onto discovered apps as `nxr.category`
and uses it for `--category` filters.

```nix
nxr.apps.api-test = {
  description = "Run API tests";
  category = "backend";
  script = ''
    exec cargo test -p api "$@"
  '';
};
```

## Optional project namespaces

Place `nxr.projects.json` at the flake root (schema
[`projects-v1`](../schemas/projects-v1.schema.json)):

```json
{
  "schema_version": 1,
  "projects": {
    "api": {
      "description": "API package",
      "apps": ["api-test", "api-lint"],
      "tasks": ["api-ci"]
    }
  }
}
```

Member names must already exist as flake apps/tasks. Unknown members are
simply absent from filtered views; the file never invents operations.

## CLI

```bash
nxr list --category backend
nxr list --namespace api
nxr inspect --category frontend
nxr inspect --namespace web --json
```

`--category` and `--namespace` may be combined (intersection).
`--namespace` requires `nxr.projects.json`.

## Fixture

See [`fixtures/namespaced-monorepo/`](../fixtures/namespaced-monorepo/) for a
two-package layout with categories and an optional projects file.
