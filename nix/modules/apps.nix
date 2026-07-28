# flake-parts module: declarative nxr.apps -> ordinary apps.<system>.*
{
  lib,
  pkgs,
  config,
  nxrSelf ? null,
  ...
}:
let
  inherit (lib) types;

  nxrLib = import ../lib { inherit pkgs; };

  fastPathType = types.submodule {
    options = {
      enable = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, local `nxr <app>` may execute the live workspace `file`
          instead of the store-backed app (ADR-0170). Remote flakes never use
          the live path. `nix run` always uses the store app.
        '';
      };

      shell = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional default devShell name for the live fast path.";
      };
    };
  };

  validateRepoRelative =
    label: value:
    if value == null then
      null
    else if lib.hasPrefix "/" value then
      throw "nxr.apps.${label}: file must be repository-relative (got absolute path)"
    else if lib.any (part: part == ".." || part == "") (lib.splitString "/" value) then
      throw "nxr.apps.${label}: file must not contain empty segments or '..' (got ${value})"
    else
      value;

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
        type = types.nullOr types.str;
        default = null;
        description = "Inline shell script body. Mutually exclusive with `file`.";
      };

      file = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Flake-root-relative workspace script path (for example `scripts/deploy.nu`).
          Mutually exclusive with `script`. Emits a store-backed app for `nix run`
          and optional live fast-path metadata for local `nxr` runs.
        '';
      };

      interpreter = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional absolute interpreter path used to run `file` (store or live).";
      };

      fastPath = lib.mkOption {
        type = fastPathType;
        default = { };
        description = "Optional local live-workspace fast path for file-backed apps.";
      };
    };
  };

  cfg = config.nxr.apps;

  fileBackedPaths = lib.filter (p: p != null) (
    lib.mapAttrsToList (
      name: app:
      validateRepoRelative name app.file
    ) cfg
  );

  mkStoreApp =
    attrName: appCfg:
    let
      fileRel = validateRepoRelative attrName appCfg.file;
      hasScript = appCfg.script != null;
      hasFile = fileRel != null;
    in
    if hasScript && hasFile then
      throw "nxr.apps.${attrName}: set exactly one of `script` or `file`"
    else if !hasScript && !hasFile then
      throw "nxr.apps.${attrName}: set `script` or `file`"
    else if hasScript then
      nxrLib.mkApp {
        inherit pkgs;
        name = attrName;
        description = appCfg.description;
        category = appCfg.category;
        runtimeInputs = appCfg.runtimeInputs;
        text = appCfg.script;
      }
    else if nxrSelf == null then
      throw "nxr.apps.${attrName}: file-backed apps require flake `self` (import via flake-parts)"
    else
      let
        srcPath = nxrSelf + "/${fileRel}";
        scriptBody = builtins.readFile srcPath;
        scriptFile = pkgs.writeScript "${attrName}-workspace-script" scriptBody;
        runLine =
          if appCfg.interpreter != null then
            ''exec ${appCfg.interpreter} ${scriptFile} "$@"''
          else
            ''exec ${scriptFile} "$@"'';
      in
      nxrLib.mkApp {
        inherit pkgs;
        name = attrName;
        description = appCfg.description;
        category = appCfg.category;
        runtimeInputs = appCfg.runtimeInputs;
        text = runLine;
      };
in
{
  options.nxr.apps = lib.mkOption {
    type = types.attrsOf appType;
    default = { };
    description = "Declarative app definitions emitted as standard flake apps.";
  };

  config = {
    apps = lib.mapAttrs mkStoreApp cfg;

    # File-backed scripts participate in discovery invalidation (ADR-0170).
    nxr.discoveryInputs = lib.mkAfter fileBackedPaths;
  };
}
