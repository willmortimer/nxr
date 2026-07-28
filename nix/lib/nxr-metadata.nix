# Compact optional `nxrMetadata.<system>` document for cold discovery.
#
# Standard flake outputs (`apps`, `packages`, `checks`, `devShells`) and
# `nxr.<system>` remain authoritative. This document is an optional accelerator
# so the CLI can load listing + orchestration metadata in one `nix eval --json`.
{ lib }:
let
  inherit (import ./task-document.nix { inherit lib; }) nxrDocument;

  outputNames =
    output:
    if output == null then
      [ ]
    else if builtins.isAttrs output then
      builtins.attrNames output
    else
      [ ];

  appDescription =
    app:
    let
      meta = app.meta or { };
    in
    if meta ? description && meta.description != null then
      meta.description
    else
      null;

  /*
    Merge listing metadata from `nxr.apps` with descriptions from standard
    `apps.<name>.meta.description` so one eval can populate `nxr list`.
  */
  appsListing =
    cfg: docApps:
    let
      fromApps = lib.mapAttrs (
        name: app:
        let
          description = appDescription app;
          fromDoc = docApps.${name} or { };
        in
        fromDoc
        // lib.optionalAttrs (description != null) {
          inherit description;
        }
      ) (cfg.apps or { });
      # Preserve doc-only listing keys that are not present as flake apps yet.
      docOnly = lib.filterAttrs (name: _: !(fromApps ? ${name})) docApps;
    in
    lib.filterAttrs (_: meta: meta != { }) (fromApps // docOnly);

  inventoryFromCfg = cfg: {
    apps = outputNames (cfg.apps or null);
    packages = outputNames (cfg.packages or null);
    checks = outputNames (cfg.checks or null);
    devShells = outputNames (cfg.devShells or null);
  };
in
{
  /*
    Metadata envelope major version. Bump when the `nxrMetadata` shape breaks.
  */
  schemaVersion = 1;

  /*
    Build one `nxrMetadata.<system>` document from per-system flake-parts config.
  */
  nxrMetadataDocument =
    {
      cfg,
      namespaces ? { },
    }:
    let
      doc = nxrDocument cfg;
      apps = appsListing cfg (doc.apps or { });
    in
    {
      schema_version = 1;
      task_schema_version = doc.schema_version;
      tasks = doc.tasks;
      inventory = inventoryFromCfg cfg;
      namespaces = namespaces;
    }
    // lib.optionalAttrs (apps != { }) {
      inherit apps;
    }
    // lib.optionalAttrs ((doc.contexts or { }) != { }) {
      contexts = doc.contexts;
    }
    // lib.optionalAttrs ((doc.discoveryInputs or [ ]) != [ ]) {
      discoveryInputs = doc.discoveryInputs;
    }
    // lib.optionalAttrs ((doc.processes or { }) != { }) {
      processes = doc.processes;
    };
}
