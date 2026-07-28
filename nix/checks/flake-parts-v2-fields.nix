# Pure smoke test for schema v2 task I/O, cache, resources, and process serialization.
{
  pkgs,
}:
let
  lib = pkgs.lib;
  inherit (import ../lib/task-document.nix { inherit lib; }) nxrDocument;

  sampleCfg = {
    nxr = {
      schemaVersion = null;
      apps = { };
      contexts = { };
      discoveryInputs = [ ];
      tasks = {
        build = {
          description = "Build with declared inputs, outputs, cache, and resources";
          app = "build";
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
          inputs = {
            paths = [ "Cargo.toml" ];
            env = [
              "RUSTFLAGS"
              {
                name = "CI";
                secret = true;
              }
            ];
            includeGitState = true;
            bindings = {
              report = {
                from = "test.junit";
              };
            };
          };
          outputs = [
            {
              path = "target/debug";
              mode = "replace";
              optional = true;
            }
          ];
          cache = {
            mode = "local";
            version = "1";
            restore = true;
            save = true;
            failures = null;
          };
          resources = {
            cpu = 2;
            memory = "4GiB";
            io = "heavy";
            network = false;
            exclusive = [ "cargo-target" ];
          };
        };
      };
      processes = {
        api = {
          app = "api";
          dependsOn = [ "database@ready" ];
          readiness = {
            tcp = null;
            http = {
              url = "http://127.0.0.1:8080/health";
            };
          };
          restart = "on-failure";
        };
      };
    };
  };

  doc = nxrDocument sampleCfg;
  build = doc.tasks.build;
in
assert doc.schema_version == 2;
assert build.inputs.paths == [ "Cargo.toml" ];
assert build.inputs.includeGitState == true;
assert build.inputs.bindings.report.from == "test.junit";
assert (builtins.elemAt build.outputs 0).path == "target/debug";
assert build.cache.mode == "local";
assert build.resources.cpu == 2;
assert build.resources.exclusive == [ "cargo-target" ];
assert doc.processes.api.app == "api";
assert doc.processes.api.restart == "on-failure";
assert doc.processes.api.readiness.http.url == "http://127.0.0.1:8080/health";
pkgs.runCommand "nxr-flake-parts-v2-fields" { } "touch $out"
