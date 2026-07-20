{
  description = "nxr fixture: multi-package monorepo with categories and namespaces";

  inputs = {
    nxr.url = "path:../..";
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
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];

      perSystem =
        { ... }:
        {
          # Two package-style namespaces sharing one flake: api + web.
          nxr.apps = {
            api-test = {
              description = "Run API tests";
              category = "backend";
              script = ''
                echo "api-test"
              '';
            };

            api-lint = {
              description = "Lint API sources";
              category = "backend";
              script = ''
                echo "api-lint"
              '';
            };

            web-test = {
              description = "Run web tests";
              category = "frontend";
              script = ''
                echo "web-test"
              '';
            };

            web-lint = {
              description = "Lint web sources";
              category = "frontend";
              script = ''
                echo "web-lint"
              '';
            };

            shared-fmt = {
              description = "Format the whole workspace";
              category = "workspace";
              script = ''
                echo "shared-fmt"
              '';
            };
          };

          nxr.tasks = {
            api-ci = {
              description = "API CI gate";
              dependsOn = [ "api-test" ];
              app = "api-lint";
              category = "backend";
            };

            api-test = {
              description = "API tests";
              app = "api-test";
              category = "backend";
            };

            web-ci = {
              description = "Web CI gate";
              dependsOn = [ "web-test" ];
              app = "web-lint";
              category = "frontend";
            };

            web-test = {
              description = "Web tests";
              app = "web-test";
              category = "frontend";
            };

            fmt = {
              description = "Workspace format";
              app = "shared-fmt";
              category = "workspace";
            };
          };
        };
    };
}
