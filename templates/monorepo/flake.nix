{
  description = "Namespaced monorepo scaffolded with nxr";

  inputs = {
    nxr.url = "github:willmortimer/nxr";
    nixpkgs.follows = "nxr/nixpkgs";
    flake-parts.follows = "nxr/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nxr, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        nxr.flakeModules.default
      ];

      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      perSystem =
        { pkgs, ... }:
        {
          nxr.apps = {
            "api-test" = {
              description = "Run API package tests";
              category = "backend";
              runtimeInputs = [ pkgs.cargo ];
              script = ''
                exec cargo test -p api "$@"
              '';
            };

            "api-lint" = {
              description = "Lint API package sources";
              category = "backend";
              runtimeInputs = [ pkgs.cargo pkgs.clippy ];
              script = ''
                exec cargo clippy -p api -- -D warnings
              '';
            };

            "web-test" = {
              description = "Run web package tests";
              category = "frontend";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm test --prefix web --if-present "$@"
              '';
            };

            "web-lint" = {
              description = "Lint web package sources";
              category = "frontend";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm run lint --prefix web --if-present "$@"
              '';
            };

            "shared-fmt" = {
              description = "Format the whole workspace";
              category = "workspace";
              runtimeInputs = [ pkgs.cargo pkgs.rustfmt ];
              script = ''
                exec cargo fmt --all
              '';
            };
          };

          nxr.tasks = {
            fmt = { app = "shared-fmt"; category = "workspace"; };
            "api-test" = { app = "api-test"; category = "backend"; };
            "api-ci" = {
              app = "api-lint";
              category = "backend";
              dependsOn = [ "api-test" ];
            };
            "web-test" = { app = "web-test"; category = "frontend"; };
            "web-ci" = {
              app = "web-lint";
              category = "frontend";
              dependsOn = [ "web-test" ];
            };
            ci = {
              app = "api-lint";
              dependsOn = [ "fmt" "api-ci" "web-ci" ];
              description = "Workspace CI gate";
            };
          };
        };
    };
}
