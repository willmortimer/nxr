# Tasks and DAGs

Tasks **coordinate** flake apps; they do not replace them.

```bash
nxr list tasks
nxr task ci
nxr task lint unit -j 8          # union DAG; shared deps run once
nxr graph ci --format mermaid
nxr task ci --dry-run
nxr task deploy-prod --set reason="ticket-123"
```

## Authoring

With `nxr.flakeModules.default`:

```nix
perSystem.nxr.tasks.ci = {
  description = "Quality gate";
  dependsOn = [ "fmt" "lint" "test" ];
};
```

Schema reference:
[TASKS.md](https://github.com/willmortimer/nxr/blob/main/docs/TASKS.md),
[TASK_SCHEMA_V2.md](https://github.com/willmortimer/nxr/blob/main/docs/TASK_SCHEMA_V2.md).

## Parameters

Typed parameters become `NXR_PARAM_*` at spawn. Required fields fail closed when
non-interactive (use `--set` / env). On a TTY, nxr can prompt (mux-aware under
tmux/zellij).

## Decision branching (wizards)

Branching stays **wizard flake app → `nxr task …`**, not a schema `when` DSL.

Demo fixture: [`fixtures/deploy-wizard`](https://github.com/willmortimer/nxr/tree/main/fixtures/deploy-wizard).

```bash
# Scripted (CI):
NXR_FIXTURE_WIZARD_ENV=staging nxr --flake fixtures/deploy-wizard deploy-wizard
NXR_FIXTURE_WIZARD_ENV=production NXR_FIXTURE_WIZARD_REASON=demo \
  nxr --flake fixtures/deploy-wizard deploy-wizard
```

Patterns: [PATTERNS.md](https://github.com/willmortimer/nxr/blob/main/docs/PATTERNS.md).
