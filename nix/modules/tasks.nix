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
        description = "Optional working-directory policy or path (for example flake-root or invocation).";
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
