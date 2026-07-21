# Entry flake-parts module for nxr consumers.
#
# Imports per-system apps, tasks, and shellIntegration modules, then emits
# versioned metadata at flake output `nxr.<system>` (TaskDocument:
# schema_version + tasks + optional apps listing metadata).
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
    }
    // lib.optionalAttrs (task.timeout != null) {
      timeout = task.timeout;
    }
    // lib.optionalAttrs (task.terminationGracePeriod != null) {
      terminationGracePeriod = task.terminationGracePeriod;
    };

  appListingToJson =
    app:
    lib.optionalAttrs (app.category != null) {
      category = app.category;
    };

  nxrDocument =
    cfg:
    let
      appsMeta = lib.filterAttrs (_: meta: meta != { }) (
        lib.mapAttrs (_name: appListingToJson) cfg.nxr.apps
      );
    in
    {
      schema_version = 1;
      tasks = lib.mapAttrs (_name: taskToJson) cfg.nxr.tasks;
    }
    // lib.optionalAttrs (appsMeta != { }) {
      apps = appsMeta;
    }
    // lib.optionalAttrs (cfg.nxr.discoveryInputs != [ ]) {
      discoveryInputs = cfg.nxr.discoveryInputs;
    };
in
{
  perSystem = {
    imports = [
      ./apps.nix
      ./tasks.nix
      ./shell-integration.nix
      ./discovery.nix
    ];
  };

  # `nxr.<system>` → { schema_version = 1; tasks = { ... }; apps?; discoveryInputs?; }
  flake.nxr = lib.mapAttrs (_system: cfg: nxrDocument cfg) config.allSystems;
}
