{
  description = "nxr fixture: deploy wizard app → deploy-staging / deploy-prod tasks";

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
          deploy-staging = {
            description = "Deploy to staging (default version param)";
            app = "deploy-staging";
            dependsOn = [ ];
            hidden = false;
            parameters = {
              version = {
                type = "string";
                default = "latest";
              };
            };
          };
          deploy-prod = {
            description = "Deploy to production (required audit reason)";
            app = "deploy-prod";
            dependsOn = [ ];
            hidden = false;
            parameters = {
              reason = {
                type = "string";
              };
              version = {
                type = "string";
                default = "latest";
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
          deploy-staging = mkApp pkgs "fixture-deploy-staging" "Simulated staging deploy leaf" ''
            echo "deploy-staging: target=staging version=''${NXR_PARAM_VERSION:-unset}"
          '';

          deploy-prod = mkApp pkgs "fixture-deploy-prod" "Simulated production deploy leaf" ''
            echo "deploy-prod: target=production version=''${NXR_PARAM_VERSION:-unset} reason=''${NXR_PARAM_REASON:-unset}"
          '';

          deploy-wizard = mkApp pkgs "fixture-deploy-wizard" "Interactive deploy wizard (branches to tasks)" ''
            set -euo pipefail

            pick_env() {
              if [ -n "''${NXR_FIXTURE_WIZARD_ENV:-}" ]; then
                printf '%s' "$NXR_FIXTURE_WIZARD_ENV"
                return 0
              fi
              if [ -t 0 ] && [ -t 2 ]; then
                PS3="environment: " >&2
                select choice in staging production; do
                  if [ -n "$choice" ]; then
                    printf '%s' "$choice"
                    return 0
                  fi
                done
              fi
              echo "deploy-wizard: non-interactive (set NXR_FIXTURE_WIZARD_ENV=staging|production)" >&2
              return 2
            }

            pick_reason() {
              if [ -n "''${NXR_FIXTURE_WIZARD_REASON:-}" ]; then
                printf '%s' "$NXR_FIXTURE_WIZARD_REASON"
                return 0
              fi
              if [ -t 0 ] && [ -t 2 ]; then
                printf 'audit reason: ' >&2
                IFS= read -r reason
                if [ -n "$reason" ]; then
                  printf '%s' "$reason"
                  return 0
                fi
              fi
              echo "deploy-wizard: production requires reason (set NXR_FIXTURE_WIZARD_REASON)" >&2
              return 2
            }

            env="$(pick_env)" || exit $?

            case "$env" in
              staging)
                echo "wizard:target=deploy-staging"
                echo "wizard:invoke=nxr task deploy-staging"
                ;;
              production)
                reason="$(pick_reason)" || exit $?
                echo "wizard:target=deploy-prod"
                echo "wizard:reason=$reason"
                echo "wizard:invoke=nxr task deploy-prod --set reason=$reason"
                ;;
              *)
                echo "deploy-wizard: unknown environment $env" >&2
                exit 2
                ;;
            esac
          '';
        }
      );

      nxr = forAllSystems (_system: nxrDoc);
    };
}
