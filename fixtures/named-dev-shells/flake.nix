{
  description = "nxr fixture: named devShells and shell-marker app";

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
          shell-marker = app {
            name = "fixture-shell-marker";
            description = "Print dev shell marker env var";
            text = ''
              if [ -z "''${NXR_FIXTURE_SHELL_MARKER:-}" ]; then
                echo "missing shell marker" >&2
                exit 1
              fi
              echo "$NXR_FIXTURE_SHELL_MARKER"
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            env.NXR_FIXTURE_SHELL_MARKER = "inside-default-shell";
          };
        }
      );
    };
}
