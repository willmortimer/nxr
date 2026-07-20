{
  description = "nxr fixture: shellIntegration-shaped devShell with stub nxr on PATH";

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
    in
    {
      # Self-contained stub: CI only checks `command -v nxr` inside the shell.
      # Full module wiring is covered by the root flake and docs examples.
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          stubNxr = pkgs.writeShellApplication {
            name = "nxr";
            text = ''
              echo "nxr-fixture-stub"
            '';
          };
        in
        {
          default = pkgs.mkShell {
            packages = [ stubNxr ];
            env.NXR_FIXTURE_SHELL_MARKER = "shell-integration";
            env.NXR_SHELL_INTEGRATION = "1";
            env.NXR_DEV_SHELL = "default";
          };
        }
      );
    };
}
