# flake-parts module: declarative nxr.tasks (orchestration metadata).
#
# Authors declare tasks under `perSystem.nxr.tasks`. The parent flake-parts
# entry module emits a versioned document at flake output `nxr.<system>`:
#
#   { schema_version = 1; tasks = { ... }; }
#
# matching the Rust `TaskDocument` / `schemas/task-v1.schema.json` contract.
{
  lib,
  ...
}:
let
  inherit (lib) types;

  taskType = types.submodule {
    options = {
      description = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional short description for listings and completion.";
      };

      dependsOn = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Task names that must complete before this task runs.";
      };

      app = lib.mkOption {
        type = types.str;
        description = "Flake app leaf name (apps.<system>.<name>) this task runs.";
      };

      workingDirectory = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional working-directory policy or flake-root-relative path.
          Accepted values: `invocation`, `flake-root`, or a relative path
          (absolute paths are rejected by the runner). CLI `--root` / `--cwd`
          override this field for every node in a task run.
        '';
      };

      hidden = lib.mkOption {
        type = types.bool;
        default = false;
        description = "When true, the task is omitted from default listings.";
      };

      category = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional logical category for grouping in listings.";
      };

      aliases = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Optional alternate names resolved by explicit task commands (`nxr task`,
          `nxr graph`, `nxr inspect task`, `nxr watch`, and `nxr plan` when the
          name is not an app). Bare `nxr <name>` remains app-only.
        '';
      };

      interactive = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, the node requires exclusive terminal access: stdin and the
          controlling TTY are inherited, the scheduler runs it alone (no
          concurrent peers), and multiplexed `--output` modes are rejected.
        '';
      };

      paths = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Optional repository-relative path roots for conservative affected
          analysis (`nxr affected`). Changes under these paths mark the task
          (and its dependents) as affected.
        '';
      };

      timeout = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional wall-clock timeout for this task's process (e.g. `10m`,
          `30s`, `500ms`). When exceeded, nxr terminates the node and records
          a `timed_out` outcome.
        '';
      };

      terminationGracePeriod = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional grace period after timeout or interrupt before SIGKILL
          (e.g. `5s`). Defaults to the runner's built-in grace when unset.
        '';
      };
    };
  };
in
{
  options.nxr.tasks = lib.mkOption {
    type = types.attrsOf taskType;
    default = { };
    description = ''
      Declarative task definitions. Emitted as the `tasks` map inside the
      versioned flake output `nxr.<system>` (see docs/TASKS.md).
    '';
  };
}
