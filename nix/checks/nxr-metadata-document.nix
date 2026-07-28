# Pure smoke test for compact `nxrMetadata` document construction.
{
  pkgs,
}:
let
  lib = pkgs.lib;
  inherit (import ../lib/nxr-metadata.nix { inherit lib; }) nxrMetadataDocument schemaVersion;

  sampleCfg = {
    apps = {
      hello = {
        type = "app";
        program = "/bin/hello";
        meta.description = "Say hello";
      };
    };
    packages = {
      pkg = { };
    };
    checks = { };
    devShells = {
      default = { };
    };
    nxr = {
      schemaVersion = null;
      apps = {
        hello = {
          category = "demo";
        };
      };
      contexts = { };
      discoveryInputs = [ ];
      tasks = {
        ci = {
          description = "CI gate";
          app = "hello";
          dependsOn = [ ];
          workingDirectory = null;
          hidden = false;
          category = null;
          aliases = [ ];
          interactive = false;
          paths = [ ];
          timeout = null;
          terminationGracePeriod = null;
          shell = null;
          context = null;
          inputs = null;
          outputs = [ ];
          cache = null;
          resources = null;
        };
      };
      processes = { };
    };
  };

  doc = nxrMetadataDocument {
    cfg = sampleCfg;
    namespaces = {
      demo = {
        apps = [ "hello" ];
        tasks = [ "ci" ];
      };
    };
  };
in
assert schemaVersion == 1;
assert doc.schema_version == 1;
assert doc.task_schema_version == 1;
assert doc.tasks.ci.app == "hello";
assert doc.inventory.apps == [ "hello" ];
assert doc.inventory.packages == [ "pkg" ];
assert doc.inventory.devShells == [ "default" ];
assert doc.apps.hello.description == "Say hello";
assert doc.apps.hello.category == "demo";
assert doc.namespaces.demo.apps == [ "hello" ];
pkgs.runCommand "nxr-metadata-document" { } "touch $out"
