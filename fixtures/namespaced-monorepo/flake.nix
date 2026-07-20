{
  description = "nxr fixture: multi-package monorepo with categories and namespaces";

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
        schema_version = 1;
        apps = {
          api-test = {
            category = "backend";
          };
          api-lint = {
            category = "backend";
          };
          web-test = {
            category = "frontend";
          };
          web-lint = {
            category = "frontend";
          };
          shared-fmt = {
            category = "workspace";
          };
        };
        tasks = {
          api-ci = {
            description = "API CI gate";
            dependsOn = [ "api-test" ];
            app = "api-lint";
            category = "backend";
            hidden = false;
          };
          api-test = {
            description = "API tests";
            app = "api-test";
            category = "backend";
            dependsOn = [ ];
            hidden = false;
          };
          web-ci = {
            description = "Web CI gate";
            dependsOn = [ "web-test" ];
            app = "web-lint";
            category = "frontend";
            hidden = false;
          };
          web-test = {
            description = "Web tests";
            app = "web-test";
            category = "frontend";
            dependsOn = [ ];
            hidden = false;
          };
          fmt = {
            description = "Workspace format";
            app = "shared-fmt";
            category = "workspace";
            dependsOn = [ ];
            hidden = false;
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
          api-test = mkApp pkgs "fixture-api-test" "Run API tests" ''
            echo "api-test"
          '';
          api-lint = mkApp pkgs "fixture-api-lint" "Lint API sources" ''
            echo "api-lint"
          '';
          web-test = mkApp pkgs "fixture-web-test" "Run web tests" ''
            echo "web-test"
          '';
          web-lint = mkApp pkgs "fixture-web-lint" "Lint web sources" ''
            echo "web-lint"
          '';
          shared-fmt = mkApp pkgs "fixture-shared-fmt" "Format the whole workspace" ''
            echo "shared-fmt"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
