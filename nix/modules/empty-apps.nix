# Stub `nxr.apps` for flakes that declare apps via `perSystem.apps` instead of `nxr.apps`.
{ lib, ... }:
{
  options.nxr.apps = lib.mkOption {
    type = lib.types.attrsOf lib.types.raw;
    default = { };
    description = "Unused when apps are declared as standard flake apps.";
  };
}
