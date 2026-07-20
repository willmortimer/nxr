{
  description = "nxr fixture: shellIntegration without duplicate package wiring";

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
        { pkgs, ... }:
        {
          nxr.shellIntegration.enable = true;

          devShells.default = pkgs.mkShell {
            env.NXR_FIXTURE_SHELL_MARKER = "shell-integration";
          };
        };
    };
}
