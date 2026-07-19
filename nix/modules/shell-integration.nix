# flake-parts module: optional devShell integration for nxr.
#
# When enabled, listed dev shells receive the nxr package, completion path
# exports, and a session-local shellHook that activates shell integration
# without writing global dotfiles.
{
  lib,
  config,
  ...
}:
let
  inherit (lib) types;

  cfg = config.nxr.shellIntegration;

  resolvePackage =
    if cfg.package != null then
      cfg.package
    else if config ? packages && config.packages ? nxr then
      config.packages.nxr
    else
      null;

  nxrPkg =
    if cfg.enable && resolvePackage == null then
      throw "nxr.shellIntegration.enable requires packages.nxr or nxr.shellIntegration.package"
    else
      resolvePackage;

  # Session-local hook shared by every integrated dev shell.
  integrationHook =
    shellName: ''
      # nxr shellIntegration (session-local; no global dotfile writes)
      if [ -z "''${NXR_SHELL_INTEGRATION:-}" ]; then
        export NXR_SHELL_INTEGRATION=1
        export NXR_DEV_SHELL=${lib.escapeShellArg shellName}
        export NXR_PACKAGE=${lib.escapeShellArg nxrPkg}
        export NXR_COMPLETION_DIR="${nxrPkg}/share"
        export XDG_DATA_DIRS="${nxrPkg}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
        export FPATH="${nxrPkg}/share/zsh/site-functions''${FPATH:+:$FPATH}"

        if [ -n "''${ZSH_VERSION:-}" ]; then
          # shellcheck disable=SC1091
          . "${nxrPkg}/share/nxr/shell/integrate.zsh"
        elif [ -n "''${BASH_VERSION:-}" ]; then
          # shellcheck disable=SC1091
          . "${nxrPkg}/share/nxr/shell/integrate.bash"
        fi
      fi
    '';

  wrapDevShell =
    shellName: shell:
    shell.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [ nxrPkg ];
      shellHook = (old.shellHook or "") + integrationHook shellName;
    });
in
{
  options.nxr.shellIntegration = {
    enable = lib.mkOption {
      type = types.bool;
      default = false;
      description = ''
        When true, augment configured dev shells with the nxr package and a
        session-local shell hook that activates completion integration.
      '';
    };

    devShells = lib.mkOption {
      type = types.listOf types.str;
      default = lib.mkDefault [ "default" ];
      description = ''
        Names of `devShells` to augment when `enable` is true.
      '';
    };

    package = lib.mkOption {
      type = types.nullOr types.package;
      default = null;
      description = ''
        nxr package installed into integrated dev shells. Defaults to
        `packages.nxr` when that attribute exists.
      '';
    };
  };

  options.devShells = lib.mkOption {
    apply =
      shells:
      if !cfg.enable then
        shells
      else
        lib.mapAttrs (
          name: shell:
          if lib.elem name cfg.devShells then
            wrapDevShell name shell
          else
            shell
        ) shells;
  };
}
