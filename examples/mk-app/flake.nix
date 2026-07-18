{
  description = "Example: nxr mkApp helper and flake-parts apps module";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
    flake-parts.follows = "nxr/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nxr, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        nxr.flakeModules.default
      ];

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];

      perSystem =
        { pkgs, ... }:
        {
          nxr.apps = {
            hello = {
              description = "Print a greeting via nxr.apps";
              script = ''
                echo "hello from examples/mk-app"
              '';
            };

            echo-args = {
              description = "Echo forwarded arguments";
              script = ''
                printf '%s\n' "$@"
              '';
            };
          };

          apps.greet = nxr.lib.mkApp {
            inherit pkgs;
            name = "example-greet";
            description = "Greet via nxr.lib.mkApp";
            text = ''
              echo "greet via lib.mkApp"
            '';
          };
        };
    };
}
