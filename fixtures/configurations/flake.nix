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
      nixosConfigurations.dev = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          ({ lib, ... }: {
            options.services.nxr-fixture-marker = lib.mkOption {
              type = lib.types.str;
              default = "configuration-fixture";
            };
          })
        ];
      };
    };
}
