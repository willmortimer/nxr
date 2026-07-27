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
    }
    // lib.optionalAttrs (task.shell != null) {
      shell = task.shell;
    }
    // lib.optionalAttrs (task.context != null) {
      context = task.context;
    };

  contextEnvironmentToJson =
    env:
    {
      mode = env.mode;
    }
    // lib.optionalAttrs (env.keep != [ ]) {
      keep = env.keep;
    }
    // lib.optionalAttrs (env.set != { }) {
      set = env.set;
    }
    // lib.optionalAttrs (env.unset != [ ]) {
      unset = env.unset;
    };

  contextToJson =
    ctx:
    lib.filterAttrs (_: v: v != null && v != { } && v != false) (
      {
        shell = ctx.shell;
        environment = if ctx.environment != null then contextEnvironmentToJson ctx.environment else null;
        secrets = lib.mapAttrs (
          _name: secret:
          {
            ref = secret.ref;
            delivery = secret.delivery;
            provider = secret.provider;
          }
        ) ctx.secrets;
        confirm = ctx.confirm;
      }
    );

  appListingToJson =
    app:
    lib.optionalAttrs (app.category != null) {
      category = app.category;
    };

  taskUsesSchemaV2 =
    task:
    task.shell != null || task.context != null;

  nxrDocument =
    cfg:
    let
      appsMeta = lib.filterAttrs (_: meta: meta != { }) (
        lib.mapAttrs (_name: appListingToJson) cfg.nxr.apps
      );
      contextsJson = lib.mapAttrs (_name: contextToJson) cfg.nxr.contexts;
      hasV2Fields =
        contextsJson != { }
        || lib.any taskUsesSchemaV2 cfg.nxr.tasks;
      forcedVersion = cfg.nxr.schemaVersion;
      schema_version =
        if forcedVersion == 2 then
          2
        else if forcedVersion == 1 && hasV2Fields then
          throw "nxr.schemaVersion = 1 but the nxr document uses schema v2 fields (contexts, task.shell, task.context, …). Set schemaVersion = 2 or remove v2 fields."
        else if hasV2Fields then
          2
        else
          1;
    in
    {
      schema_version = schema_version;
      tasks = lib.mapAttrs (_name: taskToJson) cfg.nxr.tasks;
    }
    // lib.optionalAttrs (appsMeta != { }) {
      apps = appsMeta;
    }
    // lib.optionalAttrs (contextsJson != { }) {
      contexts = contextsJson;
    }
    // lib.optionalAttrs (cfg.nxr.discoveryInputs != [ ]) {
      discoveryInputs = cfg.nxr.discoveryInputs;
    };
in
{
  imports = [
    ./schema.nix
  ];

  perSystem = {
    imports = [
      ./apps.nix
      ./tasks.nix
      ./contexts.nix
      ./shell-integration.nix
      ./discovery.nix
    ];
  };

  # `nxr.<system>` → { schema_version = 1|2; tasks = { ... }; apps?; contexts?; discoveryInputs?; }
  flake.nxr = lib.mapAttrs (_system: cfg: nxrDocument cfg) config.allSystems;
}
