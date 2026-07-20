{
  description = "nxr fixture: conservative affected analysis with shared dependency";

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
          shared-lib = {
            description = "Shared dependency check";
            app = "shared-check";
            paths = [ "shared" ];
            dependsOn = [ ];
            hidden = false;
          };
          api-test = {
            description = "API tests";
            dependsOn = [ "shared-lib" ];
            app = "api-test";
            workingDirectory = "crates/api";
            hidden = false;
          };
          web-test = {
            description = "Web tests";
            dependsOn = [ "shared-lib" ];
            app = "web-test";
            workingDirectory = "crates/web";
            hidden = false;
          };
          ci = {
            description = "CI gate";
            dependsOn = [
              "api-test"
              "web-test"
            ];
            app = "ci";
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
          shared-check = mkApp pkgs "fixture-shared-check" "Validate shared library inputs" ''
            test -f shared/lib.txt
            echo "shared ok"
          '';
          api-test = mkApp pkgs "fixture-api-test" "API package tests" ''
            test -f crates/api/README.md
            echo "api ok"
          '';
          web-test = mkApp pkgs "fixture-web-test" "Web package tests" ''
            test -f crates/web/README.md
            echo "web ok"
          '';
          ci = mkApp pkgs "fixture-ci" "CI gate" ''
            echo "ci ok"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
