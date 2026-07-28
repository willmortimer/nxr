# flake-parts module: declarative nxr.processes (long-running supervised apps).
#
# Process nodes declare flake apps to supervise with optional readiness probes,
# restart policy, and dependency ordering. Emitted on `nxr.<system>.processes`.
{
  lib,
  ...
}:
let
  inherit (lib) types;

  processRestart = types.enum [
    "never"
    "on-failure"
    "always"
  ];

  readinessType = types.submodule {
    options = {
      tcp = lib.mkOption {
        type = types.nullOr (
          types.submodule {
            options.port = lib.mkOption {
              type = types.int;
              description = "TCP port to probe for readiness.";
            };
          }
        );
        default = null;
        description = "TCP port readiness probe.";
      };

      http = lib.mkOption {
        type = types.nullOr (
          types.submodule {
            options.url = lib.mkOption {
              type = types.str;
              description = "HTTP URL to probe for readiness.";
            };
          }
        );
        default = null;
        description = "HTTP readiness probe.";
      };
    };
  };

  processType = types.submodule {
    options = {
      app = lib.mkOption {
        type = types.str;
        description = "Flake app leaf name (`apps.<system>.<name>`) this process runs.";
      };

      dependsOn = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Process or readiness dependencies (for example `database@ready`).
        '';
      };

      readiness = lib.mkOption {
        type = types.nullOr readinessType;
        default = null;
        description = "Optional readiness probe (`tcp` or `http`).";
      };

      restart = lib.mkOption {
        type = types.nullOr processRestart;
        default = null;
        description = "Restart policy when the supervised app exits.";
      };
    };
  };
in
{
  options.nxr.processes = lib.mkOption {
    type = types.attrsOf processType;
    default = { };
    description = ''
      Long-running process nodes for `nxr up` / `nxr status`. Emitted on
      `nxr.<system>.processes` for discovery and supervision.
    '';
  };
}
