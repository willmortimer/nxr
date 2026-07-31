{
  description = "nxr fixture: typed task parameters (--set / fail-closed)";

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
          param-required = {
            description = "Required string parameter (no default)";
            app = "echo-params";
            dependsOn = [ ];
            hidden = false;
            parameters = {
              reason = {
                type = "string";
              };
            };
          };
          param-demo = {
            description = "Parameters with defaults";
            app = "echo-params";
            dependsOn = [ ];
            hidden = false;
            parameters = {
              mode = {
                type = "choice";
                values = [
                  "fast"
                  "slow"
                ];
                default = "fast";
              };
              label = {
                type = "string";
                default = "fixture";
              };
            };
          };
          param-choice-required = {
            description = "Required choice parameter (no default)";
            app = "echo-params";
            dependsOn = [ ];
            hidden = false;
            parameters = {
              tier = {
                type = "choice";
                values = [
                  "staging"
                  "production"
                ];
              };
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
          echo-params = mkApp pkgs "fixture-echo-params" "Echo NXR_PARAM_*" ''
            echo "mode=''${NXR_PARAM_MODE:-unset} label=''${NXR_PARAM_LABEL:-unset} reason=''${NXR_PARAM_REASON:-unset} tier=''${NXR_PARAM_TIER:-unset}"
          '';
        }
      );

      nxr = forAllSystems (_system: nxrDoc);
    };
}
