{
  description = "nxr fixture: small task DAG (fmt → test → ci)";

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
          fmt = {
            description = "Format sources";
            app = "fmt";
            dependsOn = [ ];
            hidden = false;
          };
          test = {
            description = "Run tests";
            dependsOn = [ "fmt" ];
            app = "test";
            hidden = false;
          };
          ci = {
            description = "CI gate";
            dependsOn = [ "test" ];
            app = "ci";
            category = "validation";
            aliases = [ "check" ];
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
          fmt = mkApp pkgs "fixture-fmt" "Format sources" ''
            echo "fmt"
          '';
          test = mkApp pkgs "fixture-test" "Run tests" ''
            echo "test"
          '';
          ci = mkApp pkgs "fixture-ci" "CI entrypoint" ''
            echo "ci"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
