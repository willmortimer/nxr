# Structural smoke test for `nix/schemas/nxr.nix` (no Determinate Nix required).
{
  pkgs,
}:
let
  nxrSchema = import ../schemas/nxr.nix;
  validation = import ../lib/schema-validation.nix;

  sampleDocument = {
    schema_version = 1;
    tasks = {
      fmt = {
        description = "Format sources";
        app = "fmt";
        dependsOn = [ ];
      };
      test = {
        description = "Run tests";
        app = "test";
        dependsOn = [ "fmt" ];
      };
      broken = {
        description = "Missing dependency";
        app = "test";
        dependsOn = [ "missing" ];
      };
      no-app = {
        description = "Missing app reference";
        dependsOn = [ ];
      };
    };
  };

  sampleOutput = {
    aarch64-linux = sampleDocument;
  };

  inventory = nxrSchema.inventory sampleOutput;
  taskNodes = inventory.children.aarch64-linux.children.tasks.children;

  inherit (validation)
    taskHasApp
    taskDependenciesExist
    taskValidWorkingDirectory
    documentSupportedSchemaVersion
    ;
in
assert nxrSchema.version == 1;
assert builtins.isString nxrSchema.doc;
assert builtins.isFunction nxrSchema.inventory;
assert nxrSchema.allowIFD == false;
assert taskHasApp sampleDocument.tasks.test;
assert taskDependenciesExist sampleDocument.tasks.test sampleDocument;
assert !(taskDependenciesExist sampleDocument.tasks.broken sampleDocument);
assert !(taskHasApp sampleDocument.tasks.no-app);
assert taskValidWorkingDirectory sampleDocument.tasks.test;
assert documentSupportedSchemaVersion sampleDocument;
assert taskNodes.test.evalChecks.hasApp;
assert taskNodes.test.evalChecks.dependenciesExist;
assert !(taskNodes.broken.evalChecks.dependenciesExist);
assert !(taskNodes.no-app.evalChecks.hasApp);
pkgs.runCommand "nxr-flake-schema" { } "touch $out"
