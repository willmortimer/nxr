{
  description = "nxr fixture: long-running process nodes";

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
        pkgs: name: text:
        let
          drv = pkgs.writeShellApplication {
            inherit name text;
          };
        in
        {
          type = "app";
          program = "${drv}/bin/${name}";
        };

      nxrDoc = {
        schema_version = 2;
        tasks = { };
        processes = {
          worker = {
            app = "worker";
            readiness = {
              tcp = { port = 9876; };
            };
            restart = "on-failure";
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
          worker = mkApp pkgs "fixture-worker" ''
            echo "worker started"
            while true; do sleep 3600; done
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
