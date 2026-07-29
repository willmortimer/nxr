{
  description = "nxr fixture: flake-parts module with schema v2 task fields and processes";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
    nixpkgsIntelDarwin.follows = "nxr/nixpkgsIntelDarwin";
    flake-parts.follows = "nxr/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nxr, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake.schemas = { };

      imports = [
        nxr.flakeModules.default
      ];

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];

      perSystem =
        { pkgs, ... }:
        let
          mkApp =
            name: text:
            let
              drv = pkgs.writeShellApplication {
                inherit name text;
              };
            in
            {
              type = "app";
              program = "${drv}/bin/${name}";
            };
        in
        {
          nxr.tasks.build = {
            description = "Build with declared inputs, outputs, cache, and resources";
            app = "build";
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
              bindings.report = {
                from = "test.junit";
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
            };
            resources = {
              cpu = 2;
              memory = "4GiB";
              io = "heavy";
              network = false;
              exclusive = [ "cargo-target" ];
            };
          };

          nxr.processes.api = {
            app = "api";
            dependsOn = [ "database@ready" ];
            readiness.http.url = "http://127.0.0.1:8080/health";
            restart = "never";
          };

          nxr.tasks.param-demo = {
            description = "Typed parameters fixture (string, choice, boolean)";
            app = "param-demo";
            parameters = {
              mode = {
                type = "choice";
                values = [ "fast" "slow" ];
                default = "fast";
              };
              verbose = {
                type = "boolean";
                default = false;
              };
              label = {
                type = "string";
                default = "fixture";
              };
            };
          };

          nxr.tasks.matrix-demo = {
            description = "Matrix include fixture (os x arch)";
            app = "matrix-demo";
            matrix.include = [
              { os = "linux"; arch = "x64"; }
              { os = "macos"; arch = "arm64"; }
              { os = "linux"; arch = "arm64"; }
            ];
          };

          apps = {
            build = mkApp "fixture-build" ''
              echo "build"
            '';
            api = mkApp "fixture-api" ''
              echo "api"
            '';
            param-demo = mkApp "fixture-param-demo" ''
              echo "mode=''${NXR_PARAM_MODE:-unset} verbose=''${NXR_PARAM_VERBOSE:-unset} label=''${NXR_PARAM_LABEL:-unset}"
            '';
            matrix-demo = mkApp "fixture-matrix-demo" ''
              echo "os=${NXR_MATRIX_OS:-unset} arch=${NXR_MATRIX_ARCH:-unset}"
            '';
          };
        };
    };
}
