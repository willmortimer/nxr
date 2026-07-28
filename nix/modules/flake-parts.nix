# Entry flake-parts module for nxr consumers.
#
# Imports per-system apps, tasks, and shellIntegration modules, then emits
# versioned metadata at flake output `nxr.<system>` (TaskDocument:
# schema_version + tasks + optional apps listing metadata) and optional
# compact `nxrMetadata.<system>` for single-eval cold discovery.
{
  lib,
  config,
  ...
}:
let
  inherit (import ../lib/task-document.nix { inherit lib; }) nxrDocument;
in
{
  imports = [
    ./schema.nix
    ./metadata.nix
  ];

  perSystem = {
    imports = [
      ./apps.nix
      ./tasks.nix
      ./contexts.nix
      ./processes.nix
      ./shell-integration.nix
      ./discovery.nix
    ];
  };

  # `nxr.<system>` → { schema_version = 1|2; tasks = { ... }; apps?; contexts?; processes?; discoveryInputs?; }
  flake.nxr = lib.mapAttrs (_system: cfg: nxrDocument cfg) config.allSystems;
}
