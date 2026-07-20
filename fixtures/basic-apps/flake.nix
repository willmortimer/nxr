{
  description = "nxr fixture: basic flake apps for common task shapes";

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
          default = app {
            name = "fixture-hello";
            description = "Print a greeting";
            text = ''
              echo "hello from basic-apps"
            '';
          };

          hello = app {
            name = "fixture-hello";
            description = "Print a greeting";
            text = ''
              echo "hello from basic-apps"
            '';
          };

          echo-args = app {
            name = "fixture-echo-args";
            description = "Echo arguments after --";
            text = ''
              printf '%s\n' "$@"
            '';
          };

          succeed = app {
            name = "fixture-succeed";
            description = "Exit successfully";
            text = ''
              exit 0
            '';
          };

          fail = app {
            name = "fixture-fail";
            description = "Exit with status 42";
            text = ''
              exit 42
            '';
          };

          pwd = app {
            name = "fixture-pwd";
            description = "Print the invocation working directory";
            text = ''
              pwd
            '';
          };
        }
      );
    };
}
