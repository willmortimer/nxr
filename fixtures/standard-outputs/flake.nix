{
  description = "nxr fixture: packages, checks, and named development shells";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
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
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.writeText "fixture-package-default" "default-package\n";
          marker = pkgs.writeText "fixture-package-marker" "marker-package\n";
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          ok = pkgs.runCommand "fixture-check-ok" { } ''
            echo ok > "$out"
          '';
        }
      );

      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          drv = pkgs.writeShellApplication {
            name = "fixture-hello";
            text = ''
              echo "hello from standard-outputs"
            '';
          };
        in
        {
          hello = {
            type = "app";
            program = "${drv}/bin/fixture-hello";
            meta.description = "Print a greeting";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            env.NXR_FIXTURE_SHELL_MARKER = "inside-default-shell";
          };
          backend = pkgs.mkShell {
            env.NXR_FIXTURE_SHELL_MARKER = "inside-backend-shell";
          };
        }
      );
    };
}
