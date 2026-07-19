{
  description = "nxr fixture: small task DAG (fmt → test → ci)";

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
        "aarch64-linux"
      ];

      perSystem =
        { ... }:
        {
          nxr.apps = {
            fmt = {
              description = "Format sources";
              script = ''
                echo "fmt"
              '';
            };

            test = {
              description = "Run tests";
              script = ''
                echo "test"
              '';
            };

            ci = {
              description = "CI entrypoint";
              script = ''
                echo "ci"
              '';
            };
          };

          # fmt → test → ci
          nxr.tasks = {
            fmt = {
              description = "Format sources";
              app = "fmt";
            };

            test = {
              description = "Run tests";
              dependsOn = [ "fmt" ];
              app = "test";
            };

            ci = {
              description = "CI gate";
              dependsOn = [ "test" ];
              app = "ci";
              category = "validation";
              aliases = [ "check" ];
            };
          };
        };
    };
}
