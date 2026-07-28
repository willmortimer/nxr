{
  description = "nxr fixture: workspace action with local CAS + cache safety";

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
        pkgs: name: description: text:
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

      nxrDoc = {
        schema_version = 2;
        tasks = {
          codegen = {
            description = "Write a generated artifact";
            app = "codegen";
            workingDirectory = "flake-root";
            outputs = [
              { path = "gen/out.txt"; }
            ];
            cache = {
              mode = "local";
            };
          };
          secret-codegen = {
            description = "Secret-bearing workspace action (cache disabled by default)";
            app = "secret-codegen";
            workingDirectory = "flake-root";
            inputs = {
              env = [
                {
                  name = "NXR_FIXTURE_API_KEY";
                  secret = true;
                }
              ];
            };
            outputs = [
              { path = "gen/secret-out.txt"; }
            ];
            cache = {
              mode = "local";
            };
          };
          secret-codegen-override = {
            description = "Secret-bearing action with ignore-values cache override";
            app = "secret-codegen";
            workingDirectory = "flake-root";
            inputs = {
              env = [
                {
                  name = "NXR_FIXTURE_API_KEY";
                  secret = true;
                }
              ];
            };
            outputs = [
              { path = "gen/secret-out.txt"; }
            ];
            cache = {
              mode = "local";
              secretPolicy = "ignore-values";
            };
          };
          shared-codegen = {
            description = "Shared cache mode (rejected until transport exists)";
            app = "codegen";
            workingDirectory = "flake-root";
            outputs = [
              { path = "gen/out.txt"; }
            ];
            cache = {
              mode = "shared";
            };
          };
          shared-read-codegen = {
            description = "Shared-read cache mode (rejected until transport exists)";
            app = "codegen";
            workingDirectory = "flake-root";
            outputs = [
              { path = "gen/out.txt"; }
            ];
            cache = {
              mode = "shared-read";
            };
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
          codegen = mkApp pkgs "fixture-codegen" "Generate gen/out.txt" ''
            mkdir -p gen
            echo "generated" > gen/out.txt
          '';
          secret-codegen = mkApp pkgs "fixture-secret-codegen" "Generate secret-dependent artifact" ''
            mkdir -p gen
            echo "key=''${NXR_FIXTURE_API_KEY:-unset}" > gen/secret-out.txt
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
