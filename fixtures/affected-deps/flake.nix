{
  description = "nxr fixture: conservative affected analysis with shared dependency";

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
            shared-check = {
              description = "Validate shared library inputs";
              script = ''
                test -f shared/lib.txt
                echo "shared ok"
              '';
            };

            api-test = {
              description = "API package tests";
              script = ''
                test -f crates/api/README.md
                echo "api ok"
              '';
            };

            web-test = {
              description = "Web package tests";
              script = ''
                test -f crates/web/README.md
                echo "web ok"
              '';
            };

            ci = {
              description = "CI gate";
              script = ''
                echo "ci ok"
              '';
            };
          };

          nxr.tasks = {
            shared-lib = {
              description = "Shared dependency check";
              app = "shared-check";
              paths = [ "shared" ];
            };

            api-test = {
              description = "API tests";
              dependsOn = [ "shared-lib" ];
              app = "api-test";
              workingDirectory = "crates/api";
            };

            web-test = {
              description = "Web tests";
              dependsOn = [ "shared-lib" ];
              app = "web-test";
              workingDirectory = "crates/web";
            };

            ci = {
              description = "CI gate";
              dependsOn = [
                "api-test"
                "web-test"
              ];
              app = "ci";
              category = "validation";
            };
          };
        };
    };
}
