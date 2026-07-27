{
  description = "nxr golden fixture: apps, tasks, categories, contexts (schema v2)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      mkApp =
        pkgs: name: description: text:
        let
          drv = pkgs.writeShellApplication {
            inherit name text;
          };
        in
        {
          type = "app";
          program = "${drv}/bin/${name}";
          meta.description = description;
        };

      mkShell =
        pkgs: name:
        pkgs.mkShell {
          name = "golden-${name}";
          buildInputs = [ pkgs.coreutils ];
          shellHook = ''
            export NXR_DEV_SHELL=${name}
          '';
        };

      nxrDoc = {
        schema_version = 2;
        apps = {
          api-test = {
            category = "backend";
          };
          web-test = {
            category = "frontend";
          };
          shared-fmt = {
            category = "workspace";
          };
        };
        contexts = {
          backend = {
            shell = "backend";
            environment = {
              mode = "inherit";
              set = {
                GOLDEN_BACKEND = "1";
              };
            };
          };
          release = {
            shell = "release";
            environment = {
              mode = "clean";
              keep = [
                "HOME"
                "PATH"
              ];
              set = {
                RELEASE_CHANNEL = "stable";
              };
            };
          };
        };
        tasks = {
          fmt = {
            description = "Format workspace sources";
            app = "shared-fmt";
            category = "workspace";
            dependsOn = [ ];
            hidden = false;
          };
          api-test = {
            description = "Run API tests in backend context";
            app = "api-test";
            category = "backend";
            dependsOn = [ "fmt" ];
            hidden = false;
            context = "backend";
          };
          web-test = {
            description = "Run web tests";
            app = "web-test";
            category = "frontend";
            dependsOn = [ "fmt" ];
            hidden = false;
            shell = "release";
          };
          ci = {
            description = "Full validation gate";
            app = "shared-fmt";
            category = "validation";
            dependsOn = [
              "api-test"
              "web-test"
            ];
            hidden = false;
            aliases = [ "check" ];
          };
        };
      };
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          api-test = mkApp pkgs "golden-api-test" "API tests" ''
            echo "api-test"
          '';
          web-test = mkApp pkgs "golden-web-test" "Web tests" ''
            echo "web-test"
          '';
          shared-fmt = mkApp pkgs "golden-fmt" "Format" ''
            echo "fmt"
          '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          backend = mkShell pkgs "backend";
          release = mkShell pkgs "release";
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
