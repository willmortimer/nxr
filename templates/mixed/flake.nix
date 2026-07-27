{
  description = "Rust + Node monolith scaffolded with nxr";

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
            "rust-fmt" = {
              description = "Format Rust sources";
              runtimeInputs = [ pkgs.cargo pkgs.rustfmt ];
              script = ''
                exec cargo fmt --all
              '';
            };

            "rust-test" = {
              description = "Run Rust tests";
              runtimeInputs = [ pkgs.cargo ];
              script = ''
                exec cargo test "$@"
              '';
            };

            "node-install" = {
              description = "Install npm dependencies";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm ci
              '';
            };

            "node-test" = {
              description = "Run Node tests";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm test --if-present "$@"
              '';
            };
          };

          nxr.tasks = {
            "rust-fmt" = { app = "rust-fmt"; };
            "rust-test" = { app = "rust-test"; dependsOn = [ "rust-fmt" ]; };
            "node-install" = { app = "node-install"; };
            "node-test" = { app = "node-test"; dependsOn = [ "node-install" ]; };
            ci = {
              app = "rust-test";
              dependsOn = [ "node-test" ];
              description = "Run backend and frontend checks";
            };
          };
        };
    };
}
