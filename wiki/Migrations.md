# Migrations

Coming from `mise` or `just`? Scaffold suggestions without running recipes:

```bash
nxr migrate mise
nxr migrate justfile
```

Output is suggested `perSystem.nxr.*` / task wiring — review before adopting.

Deep how-to:
[MIGRATE_FROM_MISE_JUST.md](https://github.com/willmortimer/nxr/blob/main/docs/MIGRATE_FROM_MISE_JUST.md).

## Authoring robust apps

Prefer self-contained flake apps so `nxr` and `nix run` work outside a dirty
shell:

- `mkApp` / `mkScriptApp`
- `mkPackageApp`

[APP_AUTHORING.md](https://github.com/willmortimer/nxr/blob/main/docs/APP_AUTHORING.md),
[examples/mk-app](https://github.com/willmortimer/nxr/tree/main/examples/mk-app).
