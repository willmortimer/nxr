{
  description = "nxr fixture: apps with meta descriptions";

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
        pkgs:
        {
          name,
          description,
          text,
        }:
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
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          app = mkApp pkgs;
        in
        {
          lint = app {
            name = "fixture-lint";
            description = "Run static analysis";
            text = ''
              echo "lint ok"
            '';
          };

          test = app {
            name = "fixture-test";
            description = "Run the test suite";
            text = ''
              echo "test ok"
            '';
          };

          deploy = app {
            name = "fixture-deploy";
            description = "Deploy the current revision";
            text = ''
              echo "deploy skipped (fixture)"
            '';
          };
        }
      );
    };
}
