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
            secretPolicy = null;
          };
          resources = {
            cpu = 2;
            memory = "4GiB";
            io = "heavy";
            network = false;
            exclusive = [ "cargo-target" ];
          };
          parameters = { };
          matrix = null;
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
          restart = "never";
          context = null;
          workingDirectory = "flake-root";
          arguments = [
            "--port"
            "8080"
          ];
          shell = "backend";
        };
      };
    };
  };

  doc = nxrDocument sampleCfg;
  build = doc.tasks.build;
  v3Cfg = sampleCfg // {
    nxr = sampleCfg.nxr // {
      tasks = sampleCfg.nxr.tasks // {
        typed = {
          app = "typed";
          dependsOn = [ ];
          parameters.count = {
            type = "integer";
            default = 4;
            values = [ ];
            constraints = { minimum = 2; maximum = 8; step = 2; };
          };
        };
      };
    };
  };
  v3Doc = nxrDocument v3Cfg;
in
assert doc.schema_version == 2;
assert v3Doc.schema_version == 3;
assert v3Doc.tasks.typed.parameters.count.constraints.step == 2;
assert !(builtins.tryEval (nxrDocument (v3Cfg // { nxr = v3Cfg.nxr // { schemaVersion = 2; }; })).schema_version).success;
assert build.inputs.paths == [ "Cargo.toml" ];
assert build.inputs.includeGitState == true;
assert build.inputs.bindings.report.from == "test.junit";
assert (builtins.elemAt build.outputs 0).path == "target/debug";
assert build.cache.mode == "local";
assert build.resources.cpu == 2;
assert build.resources.exclusive == [ "cargo-target" ];
assert doc.processes.api.app == "api";
assert doc.processes.api.restart == "never";
assert doc.processes.api.readiness.http.url == "http://127.0.0.1:8080/health";
assert doc.processes.api.workingDirectory == "flake-root";
assert doc.processes.api.arguments == [
  "--port"
  "8080"
];
assert doc.processes.api.shell == "backend";
assert !(doc.processes.api ? context);
pkgs.runCommand "nxr-flake-parts-v2-fields" { } "touch $out"
