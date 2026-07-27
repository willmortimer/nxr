{
  description = "Node.js project scaffolded with nxr";

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
            install = {
              description = "Install npm dependencies";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm ci
              '';
            };

            lint = {
              description = "Run the project linter";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm run lint --if-present "$@"
              '';
            };

            test = {
              description = "Run the test suite";
              runtimeInputs = [ pkgs.nodejs ];
              script = ''
                exec npm test --if-present "$@"
              '';
            };
          };

          nxr.tasks = {
            install = { app = "install"; };
            lint = { app = "lint"; dependsOn = [ "install" ]; };
            ci = { app = "test"; dependsOn = [ "lint" ]; };
          };
        };
    };
}
