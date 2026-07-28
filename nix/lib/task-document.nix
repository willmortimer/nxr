# Build the versioned `nxr.<system>` task document from merged per-system config.
{ lib }:
let
  envInputToJson =
    env:
    if builtins.isString env then
      env
    else
      {
        name = env.name;
      }
      // lib.optionalAttrs env.required {
        required = true;
      }
      // lib.optionalAttrs env.secret {
        secret = true;
      };

  taskInputsToJson =
    inputs:
    lib.filterAttrs (_: v: v != null && v != { } && v != [ ] && v != false) {
      paths = inputs.paths;
      env = map envInputToJson inputs.env;
      includeGitState = inputs.includeGitState;
      bindings = lib.mapAttrs (_: binding: { from = binding.from; }) inputs.bindings;
    };

  taskInputsPresent =
    inputs:
    inputs.paths != [ ]
    || inputs.env != [ ]
    || inputs.includeGitState
    || inputs.bindings != { };

  taskOutputToJson =
    output:
    {
      path = output.path;
    }
    // lib.optionalAttrs (output.mode != null) {
      mode = output.mode;
    }
    // lib.optionalAttrs output.optional {
      optional = true;
    };

  taskCacheToJson =
    cache:
    lib.filterAttrs (_: v: v != null) {
      mode = cache.mode;
      version = cache.version;
      restore = cache.restore;
      save = cache.save;
      failures = cache.failures;
    };

  taskResourcesToJson =
    resources:
    lib.filterAttrs (_: v: v != null && v != [ ]) {
      cpu = resources.cpu;
      memory = resources.memory;
      io = resources.io;
      network = resources.network;
      exclusive = resources.exclusive;
    };

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
    }
    // lib.optionalAttrs (task.inputs != null && taskInputsPresent task.inputs) {
      inputs = taskInputsToJson task.inputs;
    }
    // lib.optionalAttrs (task.outputs != [ ]) {
      outputs = map taskOutputToJson task.outputs;
    }
    // lib.optionalAttrs (task.cache != null && taskCacheToJson task.cache != { }) {
      cache = taskCacheToJson task.cache;
    }
    // lib.optionalAttrs (task.resources != null && taskResourcesToJson task.resources != { }) {
      resources = taskResourcesToJson task.resources;
    };

  processReadinessToJson =
    readiness:
    lib.filterAttrs (_: v: v != null) {
      tcp =
        if readiness.tcp != null then
          {
            port = readiness.tcp.port;
          }
        else
          null;
      http =
        if readiness.http != null then
          {
            url = readiness.http.url;
          }
        else
          null;
    };

  processToJson =
    process:
    {
      app = process.app;
      dependsOn = process.dependsOn;
    }
    // lib.optionalAttrs (process.readiness != null && processReadinessToJson process.readiness != { }) {
      readiness = processReadinessToJson process.readiness;
    }
    // lib.optionalAttrs (process.restart != null) {
      restart = process.restart;
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
    task.shell != null
    || task.context != null
    || (task.inputs != null && taskInputsPresent task.inputs)
    || task.outputs != [ ]
    || (task.cache != null && taskCacheToJson task.cache != { })
    || (task.resources != null && taskResourcesToJson task.resources != { });
in
{
  inherit taskUsesSchemaV2 taskInputsPresent;

  nxrDocument =
    cfg:
    let
      appsMeta = lib.filterAttrs (_: meta: meta != { }) (
        lib.mapAttrs (_name: appListingToJson) cfg.nxr.apps
      );
      contextsJson = lib.mapAttrs (_name: contextToJson) cfg.nxr.contexts;
      hasV2Fields =
        contextsJson != { }
        || cfg.nxr.processes != { }
        || lib.any taskUsesSchemaV2 cfg.nxr.tasks;
      forcedVersion = cfg.nxr.schemaVersion;
      schema_version =
        if forcedVersion == 2 then
          2
        else if forcedVersion == 1 && hasV2Fields then
          throw "nxr.schemaVersion = 1 but the nxr document uses schema v2 fields (contexts, processes, task.shell, task.context, task.inputs, task.outputs, task.cache, task.resources, …). Set schemaVersion = 2 or remove v2 fields."
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
    }
    // lib.optionalAttrs (cfg.nxr.processes != { }) {
      processes = lib.mapAttrs (_name: processToJson) cfg.nxr.processes;
    };
}
