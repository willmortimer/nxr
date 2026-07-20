{
  description = "nxr fixture: multi-root union with shared diamond ancestor";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
    nixpkgsIntelDarwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
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
        "aarch64-linux"
      ];

      perSystem =
        { ... }:
        {
          nxr.apps = {
            shared = {
              description = "Shared diamond root";
              script = ''
                echo "shared"
              '';
            };

            lint = {
              description = "Lint leaf";
              script = ''
                echo "lint"
              '';
            };

            unit = {
              description = "Unit leaf";
              script = ''
                echo "unit"
              '';
            };

            integration = {
              description = "Integration join";
              script = ''
                echo "integration"
              '';
            };
          };

          # shared → (lint || unit) → integration
          nxr.tasks = {
            shared = {
              description = "Shared ancestor";
              app = "shared";
            };

            lint = {
              description = "Lint";
              dependsOn = [ "shared" ];
              app = "lint";
            };

            unit = {
              description = "Unit tests";
              dependsOn = [ "shared" ];
              app = "unit";
            };

            integration = {
              description = "Integration tests";
              dependsOn = [ "lint" "unit" ];
              app = "integration";
              category = "validation";
            };
          };
        };
    };
}
