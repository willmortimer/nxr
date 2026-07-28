# flake-parts module: optional compact `nxrMetadata.<system>` discovery endpoint.
{
  lib,
  config,
  ...
}:
let
  inherit (lib) types;
  inherit (import ../lib/nxr-metadata.nix { inherit lib; }) nxrMetadataDocument;

  namespaceType = types.submodule {
    options = {
      description = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional short description for humans.";
      };

      category = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional category label for documentation.";
      };

      apps = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Flake app leaf names belonging to this namespace.";
      };

      tasks = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Task names belonging to this namespace.";
      };
    };
  };

  namespaceToJson =
    ns:
    lib.filterAttrs (_: v: v != null && v != [ ]) {
      description = ns.description;
      category = ns.category;
      apps = ns.apps;
      tasks = ns.tasks;
    };
in
{
  options.nxr.metadata = {
    enable = lib.mkOption {
      type = types.bool;
      default = true;
      description = ''
        Emit optional `nxrMetadata.<system>` — a compact JSON-serializable
        discovery document (apps/tasks/processes/contexts/inventory/namespaces).

        NXR prefers this attribute when present so cold discovery can use one
        targeted `nix eval --json`. Standard flake outputs and `nxr.<system>`
        remain authoritative; disable when you manage `nxrMetadata` yourself or
        want to omit the accelerator.
      '';
    };
  };

  options.nxr.namespaces = lib.mkOption {
    type = types.attrsOf namespaceType;
    default = { };
    description = ''
      Optional non-authoritative namespace views embedded in `nxrMetadata`.
      Same membership shape as `nxr.projects.json` projects. Does not define
      flake apps or tasks; members must already exist as flake outputs.
    '';
  };

  config = lib.mkIf config.nxr.metadata.enable {
    flake.nxrMetadata = lib.mapAttrs (
      _system: cfg:
      nxrMetadataDocument {
        inherit cfg;
        namespaces = lib.mapAttrs (_: namespaceToJson) config.nxr.namespaces;
      }
    ) config.allSystems;
  };
}
