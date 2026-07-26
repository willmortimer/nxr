# Pure helpers for the NXR flake schema inventory `evalChecks`.
{
  taskHasApp =
    task:
    task ? app && builtins.isString task.app && task.app != "";

  taskDependenciesExist =
    task: document:
    let
      deps = task.dependsOn or [ ];
      tasks = document.tasks or { };
    in
    builtins.all (dep: builtins.hasAttr dep tasks) deps;

  taskValidWorkingDirectory =
    task:
    let
      value = task.workingDirectory or "invocation";
    in
    builtins.isString value
    && (
      value == "invocation"
      || value == "flake-root"
      || (builtins.match "/.*" value == null && builtins.match ".*\\.\\..*" value == null)
    );

  documentSupportedSchemaVersion =
    document:
    (document.schema_version or 1) == 1;
}
