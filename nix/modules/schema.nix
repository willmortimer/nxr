# flake-parts module: export the NXR custom flake schema for `nxr.<system>`.
{
  lib,
  inputs ? { },
  config,
  ...
}:
let
  nxrTaskSchema = import ../schemas/nxr.nix;
  hasFlakeSchemas = builtins.hasAttr "flake-schemas" inputs;
in
{
  options.nxr.schema = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Export the NXR flake schema at `exportedSchemas.nxr` so schema-aware
        Nix (`nix flake show`, `nix flake check`, FlakeHub) can list and
        structurally validate `nxr.<system>` task metadata.

        Disable when you manage `exportedSchemas` / `schemas` manually.
      '';
    };

    mergeIntoSchemas = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        When `inputs.flake-schemas` is present, merge the NXR schema into
        `flake.schemas` together with the standard exported schemas.

        Disable when you define `flake.schemas` yourself and merge manually
        (for example `inputs.flake-schemas.exportedSchemas // inputs.nxr.exportedSchemas`).
      '';
    };
  };

  config = {
    flake.exportedSchemas.nxr = lib.mkIf config.nxr.schema.enable nxrTaskSchema;

    flake.schemas = lib.mkIf (
      config.nxr.schema.enable && config.nxr.schema.mergeIntoSchemas && hasFlakeSchemas
    ) (lib.mkDefault (
      inputs.flake-schemas.exportedSchemas
      // {
        nxr = nxrTaskSchema;
      }
    ));
  };
}
