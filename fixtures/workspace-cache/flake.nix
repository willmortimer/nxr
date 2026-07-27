{
  description = "nxr fixture: workspace action with local CAS";

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
        schema_version = 2;
        tasks = {
          codegen = {
            description = "Write a generated artifact";
            app = "codegen";
            workingDirectory = "flake-root";
            outputs = [
              { path = "gen/out.txt"; }
            ];
            cache = {
              mode = "local";
            };
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
          codegen = mkApp pkgs "fixture-codegen" "Generate gen/out.txt" ''
            mkdir -p gen
            echo "generated" > gen/out.txt
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
