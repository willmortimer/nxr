{
  description = "nxr fixture: shellIntegration devShell wiring";

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
        { system, pkgs, ... }:
        let
          nxrPkg = nxr.packages.${system}.nxr;
        in
        {
          packages.nxr = nxrPkg;

          nxr.shellIntegration = {
            enable = true;
            devShells = [ "default" ];
            package = nxrPkg;
          };

          devShells.default = pkgs.mkShell {
            env.NXR_FIXTURE_SHELL_MARKER = "shell-integration";
          };
        };
    };
}
