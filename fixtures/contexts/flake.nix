{
  description = "nxr fixture: named execution contexts and task context refs";

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
        contexts = {
          backend = {
            shell = "backend";
            environment = {
              mode = "inherit";
              set = {
                RUST_LOG = "debug";
              };
            };
          };
          release = {
            shell = "release";
            environment = {
              mode = "clean";
              keep = [
                "HOME"
                "SSH_AUTH_SOCK"
              ];
              set = {
                RELEASE_CHANNEL = "stable";
              };
            };
            secrets = {
              DEPLOY_TOKEN = {
                ref = "NXR_FIXTURE_DEPLOY_TOKEN";
                delivery = "env";
              };
            };
            confirm = true;
          };
        };
        tasks = {
          deploy = {
            description = "Deploy with release context";
            app = "deploy";
            dependsOn = [ ];
            hidden = false;
            context = "release";
          };
          integration = {
            description = "Integration tests in backend shell";
            app = "test";
            dependsOn = [ ];
            hidden = false;
            shell = "backend";
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
          deploy = mkApp pkgs "fixture-deploy" "Deploy" ''
            if [ -z "''${DEPLOY_TOKEN:-}" ]; then
              echo "missing deploy token" >&2
              exit 42
            fi
            echo "deploy ok"
          '';
          test = mkApp pkgs "fixture-test" "Run tests" ''
            echo "test"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
