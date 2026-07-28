{
  description = "nxr fixture: package-backed app for store-exe source invalidation";

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

      mkGreet =
        pkgs:
        let
          drv = pkgs.stdenv.mkDerivation {
            name = "fixture-greet";
            src = ./src;
            dontUnpack = false;
            installPhase = ''
              mkdir -p $out/bin
              msg=$(cat message.txt)
              cat > $out/bin/fixture-greet <<EOF
              #!/bin/sh
              echo "$msg"
              EOF
              chmod +x $out/bin/fixture-greet
            '';
          };
        in
        {
          type = "app";
          program = "${drv}/bin/fixture-greet";
          meta.description = "Print greeting from packaged src/message.txt";
        };

      nxrDoc = {
        schema_version = 1;
        discoveryInputs = [ "src" ];
        tasks = { };
      };
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = mkGreet pkgs;
          greet = mkGreet pkgs;
        }
      );

      nxr = forAllSystems (_system: nxrDoc);
    };
}
