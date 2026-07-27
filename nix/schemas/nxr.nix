# Flake schema for the custom `nxr.<system>` output (Determinate flake-schemas).
#
# Runtime task discovery remains authoritative via `nix eval .#nxr.<system>`.
# This schema exposes inventory and structural `evalChecks` for `nix flake show`
# and `nix flake check` on schema-aware Nix.
let
  validation = import ../lib/schema-validation.nix;
in
{
  version = 1;

  doc = ''
    The `nxr.<system>` output contains versioned NXR application-listing and
    task-orchestration metadata. Standard flake apps remain the executable leaves.

    Evaluate `nxr.<system>` for the complete task document (dependencies,
    aliases, paths, timeouts, and other execution metadata). Schema inventory is
    for listing, inspection, and early structural diagnostics only.
  '';

  allowIFD = false;

  roles = {
    nxr-workflow = { };
  };

  inventory =
    output:
    {
      children = builtins.mapAttrs (
        system: document:
        {
          forSystems = [ system ];

          children = {
            tasks = {
              children = builtins.mapAttrs (
                name: task:
                {
                  description = task.description or null;
                  what = "NXR task";
                  shortDescription = task.description or "";
                  forSystems = [ system ];

                  evalChecks = {
                    hasApp = validation.taskHasApp task;
                    dependenciesExist = validation.taskDependenciesExist task document;
                    validWorkingDirectory = validation.taskValidWorkingDirectory task;
                    supportedSchemaVersion = validation.documentSupportedSchemaVersion document;
                    contextExists = validation.taskContextExists task document;
                  };
                }
              ) (document.tasks or { });
            };
          };
        }
      ) output;
    };
}
