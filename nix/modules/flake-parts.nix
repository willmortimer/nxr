# Entry flake-parts module for nxr consumers.
#
# Imports per-system apps, tasks, and shellIntegration modules, then emits
# versioned task metadata at flake output `nxr.<system>` (TaskDocument:
# schema_version + tasks).
{
  lib,
  config,
  ...
}:
let
  # Strip null optional fields so `nix eval --json` matches the JSON schema
  # vocabulary (dependsOn / workingDirectory) without noisy nulls.
  taskToJson =
    task:
    {
      app = task.app;
      dependsOn = task.dependsOn;
      hidden = task.hidden;
    }
    // lib.optionalAttrs (task.description != null) {
      description = task.description;
    }
    // lib.optionalAttrs (task.workingDirectory != null) {
      workingDirectory = task.workingDirectory;
    }
    // lib.optionalAttrs (task.category != null) {
      category = task.category;
    }
    // lib.optionalAttrs (task.aliases != [ ]) {
      aliases = task.aliases;
    }
    // lib.optionalAttrs task.interactive {
      interactive = task.interactive;
    }
    // lib.optionalAttrs (task.paths != [ ]) {
      paths = task.paths;
    };

  taskDocument = tasksCfg: {
    schema_version = 1;
    tasks = lib.mapAttrs (_name: taskToJson) tasksCfg;
  };
in
{
  perSystem = {
    imports = [
      ./apps.nix
      ./tasks.nix
      ./shell-integration.nix
    ];
  };

  # `nxr.<system>` → { schema_version = 1; tasks = { ... }; }
  flake.nxr = lib.mapAttrs (
    _system: cfg: taskDocument cfg.nxr.tasks
  ) config.allSystems;
}
