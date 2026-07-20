# flake-parts module: declarative nxr.apps -> ordinary apps.<system>.*
{
  lib,
  pkgs,
  config,
  ...
}:
let
  inherit (lib) types;

  nxrLib = import ../lib { inherit pkgs; };

  appType = types.submodule {
    options = {
      description = lib.mkOption {
        type = types.str;
        description = "Short imperative description shown by nix flake show and nxr list.";
      };

      category = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional logical category for list/inspect filtering (nxr.<system>.apps metadata).";
      };

      runtimeInputs = lib.mkOption {
        type = types.listOf types.package;
        default = [ ];
        description = "Packages available on PATH when the app runs.";
      };

      script = lib.mkOption {
        type = types.str;
        description = "Shell script body. Use exec and \"$@\" when forwarding arguments.";
      };
    };
  };

  cfg = config.nxr.apps;
in
{
  options.nxr.apps = lib.mkOption {
    type = types.attrsOf appType;
    default = { };
    description = "Declarative app definitions emitted as standard flake apps.";
  };

  config.apps = lib.mapAttrs (
    attrName: appCfg:
    nxrLib.mkApp {
      inherit pkgs;
      name = attrName;
      description = appCfg.description;
      category = appCfg.category;
      runtimeInputs = appCfg.runtimeInputs;
      text = appCfg.script;
    }
  ) cfg;
}
