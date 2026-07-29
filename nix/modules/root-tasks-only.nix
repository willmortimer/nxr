# Minimal flake-parts slice for nxr self-dogfood: `perSystem.nxr.tasks` authoring
# and `flake.nxr.<system>` emission without schema/metadata/shell-integration wiring.
{
  self,
  lib,
  config,
  ...
}:
let
  inherit (import ../lib/task-document.nix { inherit lib; }) nxrDocument;
in
{
  perSystem = {
    _module.args.nxrSelf = self;
    imports = [
      ./empty-apps.nix
      ./discovery.nix
      ./contexts.nix
      ./processes.nix
      ./tasks.nix
    ];
  };

  flake.nxr = lib.mapAttrs (_system: cfg: nxrDocument cfg) config.allSystems;
}
