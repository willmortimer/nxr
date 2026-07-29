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
      secretPolicy = cache.secretPolicy;
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

  taskParameterToJson =
    param:
    {
      type = param.type;
    }
    // lib.optionalAttrs (param.default != null) {
      default = param.default;
    }
    // lib.optionalAttrs (param.values != [ ]) {
      values = param.values;
    };

  taskMatrixToJson =
    matrix:
    {
      include = matrix.include;
    };

  taskToJson =
    task:
    let
      # Defensive defaults: smoke fixtures and partial task attrs may omit
      # options that the flake-parts submodule would otherwise default.
      parameters = task.parameters or { };
      matrix = task.matrix or null;
      outputs = task.outputs or [ ];
      paths = task.paths or [ ];
      aliases = task.aliases or [ ];
      interactive = task.interactive or false;
      shell = task.shell or null;
      context = task.context or null;
      inputs = task.inputs or null;
      cache = task.cache or null;
      resources = task.resources or null;
    in
    {
      app = task.app;
      dependsOn = task.dependsOn;
      hidden = task.hidden or false;
    }
    // lib.optionalAttrs ((task.description or null) != null) {
      description = task.description;
    }
    // lib.optionalAttrs ((task.workingDirectory or null) != null) {
      workingDirectory = task.workingDirectory;
    }
    // lib.optionalAttrs ((task.category or null) != null) {
      category = task.category;
    }
    // lib.optionalAttrs (aliases != [ ]) {
      inherit aliases;
    }
    // lib.optionalAttrs interactive {
      inherit interactive;
    }
    // lib.optionalAttrs (paths != [ ]) {
      inherit paths;
    }
    // lib.optionalAttrs ((task.timeout or null) != null) {
      timeout = task.timeout;
    }
    // lib.optionalAttrs ((task.terminationGracePeriod or null) != null) {
      terminationGracePeriod = task.terminationGracePeriod;
    }
    // lib.optionalAttrs (shell != null) {
      inherit shell;
    }
    // lib.optionalAttrs (context != null) {
      inherit context;
    }
    // lib.optionalAttrs (inputs != null && taskInputsPresent inputs) {
      inputs = taskInputsToJson inputs;
    }
    // lib.optionalAttrs (outputs != [ ]) {
      outputs = map taskOutputToJson outputs;
    }
    // lib.optionalAttrs (cache != null && taskCacheToJson cache != { }) {
      cache = taskCacheToJson cache;
    }
    // lib.optionalAttrs (resources != null && taskResourcesToJson resources != { }) {
      resources = taskResourcesToJson resources;
    }
    // lib.optionalAttrs (parameters != { }) {
      parameters = lib.mapAttrs (_: param: taskParameterToJson param) parameters;
    }
    // lib.optionalAttrs (matrix != null) {
      matrix = taskMatrixToJson matrix;
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
    }
    // lib.optionalAttrs (process.context != null) {
      context = process.context;
    }
    // lib.optionalAttrs (process.workingDirectory != null) {
      workingDirectory = process.workingDirectory;
    }
    // lib.optionalAttrs (process.arguments != [ ]) {
      arguments = process.arguments;
    }
    // lib.optionalAttrs (process.shell != null) {
      shell = process.shell;
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
    let
      # `empty-apps.nix` uses `types.raw`; tolerate missing attrs.
      category = app.category or null;
      runtimeInputs = app.runtimeInputs or [ ];
      file = app.file or null;
      interpreter = app.interpreter or null;
      fastPath = app.fastPath or { };
    in
    lib.filterAttrs (_: v: v != null && v != { } && v != false) (
      lib.optionalAttrs (category != null) {
        inherit category;
      }
      // lib.optionalAttrs (runtimeInputs != [ ]) {
        runtime_path = lib.makeBinPath runtimeInputs;
      }
      // lib.optionalAttrs (file != null) {
        workspace_path = file;
      }
      // lib.optionalAttrs (interpreter != null) {
        inherit interpreter;
      }
      // lib.optionalAttrs (file != null) {
        fastPath = lib.filterAttrs (_: v: v != null && v != false) {
          enable = fastPath.enable or false;
          shell = fastPath.shell or null;
        };
      }
    );

  taskUsesSchemaV2 =
    task:
    (task.shell or null) != null
    || (task.context or null) != null
    || ((task.inputs or null) != null && taskInputsPresent task.inputs)
    || (task.outputs or [ ]) != [ ]
    || ((task.cache or null) != null && taskCacheToJson task.cache != { })
    || ((task.resources or null) != null && taskResourcesToJson task.resources != { })
    || (task.parameters or { }) != { }
    || (task.matrix or null) != null;
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
        || lib.any taskUsesSchemaV2 (lib.attrValues cfg.nxr.tasks);
      forcedVersion = cfg.nxr.schemaVersion;
      schema_version =
        if forcedVersion == 2 then
          2
        else if forcedVersion == 1 && hasV2Fields then
          throw "nxr.schemaVersion = 1 but the nxr document uses schema v2 fields (contexts, processes, task.shell, task.context, task.inputs, task.outputs, task.cache, task.resources, task.parameters, task.matrix, …). Set schemaVersion = 2 or remove v2 fields."
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
