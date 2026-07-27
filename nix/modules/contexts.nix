# flake-parts module: declarative nxr.contexts (named execution contexts).
#
# Contexts bundle shell selection, environment policy, secret references
# (names only — never values), and confirmation policy. Runtime secret
# delivery is implemented separately (see docs/EXECUTION_CONTEXT.md).
{
  lib,
  ...
}:
let
  inherit (lib) types;

  environmentMode = types.enum [
    "inherit"
    "clean"
  ];

  secretDelivery = types.enum [
    "env"
    "file"
    "stdin"
  ];

  environmentType = types.submodule {
    options = {
      mode = lib.mkOption {
        type = environmentMode;
        default = "inherit";
        description = ''
          Environment inheritance policy for this context. `inherit` keeps the
          caller environment; `clean` starts from the documented clean
          allowlist and applies `keep` / `set` / `unset`.
        '';
      };

      keep = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Extra environment variable names retained in `clean` mode.";
      };

      set = lib.mkOption {
        type = types.attrsOf types.str;
        default = { };
        description = ''
          Non-secret environment values applied in this context. Secret values
          must use `secrets` references instead.
        '';
      };

      unset = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Environment variable names removed before spawn in `clean` mode.";
      };
    };
  };

  secretRefType = types.submodule {
    options = {
      ref = lib.mkOption {
        type = types.str;
        description = ''
          Logical secret reference (for example `openseat/prod/cloudflare-token`).
          Resolved at runtime via user configuration — never evaluated in Nix.
        '';
      };

      delivery = lib.mkOption {
        type = secretDelivery;
        default = "env";
        description = ''
          How the secret is delivered to the child process (`env`, `file`, or
          `stdin`). Defaults to `env`.
        '';
      };
    };
  };

  contextType = types.submodule {
    options = {
      shell = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional `devShells.<name>` to enter before running tasks that use
          this context.
        '';
      };

      environment = lib.mkOption {
        type = types.nullOr environmentType;
        default = null;
        description = "Optional environment policy for this context.";
      };

      secrets = lib.mkOption {
        type = types.attrsOf secretRefType;
        default = { };
        description = ''
          Secret requirements keyed by environment variable or file slot name.
          Values are logical references only — never secret material.
        '';
      };

      confirm = lib.mkOption {
        type = types.bool;
        default = false;
        description = "When true, the runner should prompt before execution.";
      };
    };
  };
in
{
  options.nxr.contexts = lib.mkOption {
    type = types.attrsOf contextType;
    default = { };
    description = ''
      Named execution contexts (shell, environment, secret references, confirm).
      Emitted on `nxr.<system>.contexts` for discovery and validation.
    '';
  };
}
