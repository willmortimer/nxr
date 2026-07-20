{
  description = "nxr fixture: multi-root union with shared diamond ancestor";

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

      nxrDoc = {
        schema_version = 1;
        tasks = {
          shared = {
            description = "Shared ancestor";
            app = "shared";
            dependsOn = [ ];
            hidden = false;
          };
          lint = {
            description = "Lint";
            dependsOn = [ "shared" ];
            app = "lint";
            hidden = false;
          };
          unit = {
            description = "Unit tests";
            dependsOn = [ "shared" ];
            app = "unit";
            hidden = false;
          };
          integration = {
            description = "Integration tests";
            dependsOn = [
              "lint"
              "unit"
            ];
            app = "integration";
            category = "validation";
            hidden = false;
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
          shared = mkApp pkgs "fixture-shared" "Shared diamond root" ''
            echo "shared"
          '';
          lint = mkApp pkgs "fixture-lint" "Lint leaf" ''
            echo "lint"
          '';
          unit = mkApp pkgs "fixture-unit" "Unit leaf" ''
            echo "unit"
          '';
          integration = mkApp pkgs "fixture-integration" "Integration join" ''
            echo "integration"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
