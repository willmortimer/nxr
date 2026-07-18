# Shared metadata helpers for standard flake apps and optional nxr output.
{ lib }:

let
  inherit (lib) optionalAttrs;
in
{
  /*
    Build the `meta` attribute for a flake app.

    Standard fields are promoted into `meta.description` for `nix flake show`
    and nxr list output. Additional keys are passed through for future nxr
    metadata consumers.
  */
  mkAppMeta =
    {
      description,
      ...
    }@args:
    optionalAttrs (description != null) { inherit description; }
    // removeAttrs args [ "description" ];

  schemaVersion = 1;

  /*
    Build one entry in the versioned `nxr.<system>.apps` metadata map.
  */
  mkAppsMetadataEntry =
    {
      description,
      ...
    }@args:
    { inherit description; } // removeAttrs args [ "description" ];
}
