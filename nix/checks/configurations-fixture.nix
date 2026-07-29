# Force-eval the configurations fixture's NixOS module shape on every
# `nix flake check` host. Darwin `nix flake show` filters x86_64-linux
# nixosConfigurations, so CLI tests alone cannot catch assertion failures
# that only appear on Linux CI.
#
# Keep the module body in sync with fixtures/configurations/flake.nix.
{ pkgs, nixpkgs }:
let
  eval = nixpkgs.lib.nixosSystem {
    system = "x86_64-linux";
    modules = [
      (
        { lib, ... }:
        {
          options.services.nxr-fixture-marker = lib.mkOption {
            type = lib.types.str;
            default = "configuration-fixture";
          };
          config = {
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "nodev";
              fsType = "tmpfs";
            };
            system.stateVersion = "25.05";
          };
        }
      )
    ];
  };
in
# writeText only needs option values (pure eval) — no Linux drv build on Darwin.
pkgs.writeText "nxr-configurations-fixture-eval" ''
  ${eval.config.system.stateVersion}
  ${eval.config.services.nxr-fixture-marker}
''
