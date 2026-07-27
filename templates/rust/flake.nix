{
  description = "Rust project scaffolded with nxr";

  inputs = {
    nxr.url = "github:willmortimer/nxr";
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
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      perSystem =
        { pkgs, ... }:
        {
          nxr.apps = {
            fmt = {
              description = "Format Rust sources";
              runtimeInputs = [ pkgs.cargo pkgs.rustfmt ];
              script = ''
                exec cargo fmt --all
              '';
            };

            lint = {
              description = "Run clippy with warnings denied";
              runtimeInputs = [ pkgs.cargo pkgs.clippy ];
              script = ''
                exec cargo clippy --all-targets -- -D warnings
              '';
            };

            test = {
              description = "Run the Rust test suite";
              runtimeInputs = [ pkgs.cargo ];
              script = ''
                exec cargo test "$@"
              '';
            };
          };

          nxr.tasks = {
            fmt = { app = "fmt"; };
            lint = { app = "lint"; dependsOn = [ "fmt" ]; };
            ci = { app = "test"; dependsOn = [ "lint" ]; };
          };
        };
    };
}
