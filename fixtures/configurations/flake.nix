{
  description = "nxr fixture: minimal nixosConfigurations for list/inspect/build adapters";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
    in
    {
      # Keep this evaluable on Linux `nix flake show` (Darwin filters the attr).
      # Minimal boot/rootfs so NixOS module assertions pass without a real machine.
      nixosConfigurations.dev = nixpkgs.lib.nixosSystem {
        inherit system;
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
    };
}
