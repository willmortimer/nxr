{
  description = "nxr fixture: self-contained nxrMetadata.<system> discovery endpoint";

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

      # Mirrors the flake-parts `nxrMetadata` envelope (ADR-0166). Module emission
      # is covered by `nix/checks/nxr-metadata-document.nix`; this fixture stays
      # free of `path:../..` so CLI `path:` refs work under pure eval.
      nxrDoc = {
        schema_version = 1;
        tasks = {
          ci = {
            description = "CI gate";
            app = "hello";
            dependsOn = [ ];
            hidden = false;
          };
        };
        apps = {
          hello = {
            category = "demo";
          };
        };
      };

      metadataDoc = {
        schema_version = 1;
        task_schema_version = 1;
        apps = {
          hello = {
            description = "Say hello";
            category = "demo";
          };
        };
        tasks = nxrDoc.tasks;
        inventory = {
          apps = [ "hello" ];
          packages = [ ];
          checks = [ ];
          devShells = [ "default" ];
        };
        namespaces = {
          demo = {
            description = "Demo namespace";
            apps = [ "hello" ];
            tasks = [ "ci" ];
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
          hello = mkApp pkgs "fixture-hello" "Say hello" ''
            echo "hello from nxr-metadata"
          '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell { packages = [ ]; };
        }
      );

      nxr = forAllSystems (_: nxrDoc);
      nxrMetadata = forAllSystems (_: metadataDoc);
    };
}
