{
  description = "nxr fixture: nested directory under a flake root";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
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
            name = "fixture-nested-pwd";
            description = "Print pwd (preserve invocation CWD)";
            text = ''
              pwd
            '';
          };

          pwd = app {
            name = "fixture-nested-pwd";
            description = "Print pwd (preserve invocation CWD)";
            text = ''
              pwd
            '';
          };

          root-marker = app {
            name = "fixture-root-marker";
            description = "Confirm the flake root marker file is readable";
            text = ''
              root="$PWD"
              while [[ "$root" != "/" ]]; do
                if [[ -f "$root/flake.nix" ]]; then
                  break
                fi
                root="$(dirname "$root")"
              done
              test -f "$root/ROOT_MARKER"
              echo "found ROOT_MARKER at $root"
            '';
          };
        }
      );
    };
}
