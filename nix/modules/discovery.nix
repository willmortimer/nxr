# flake-parts module: discovery cache extra inputs.
{
  lib,
  ...
}:
let
  inherit (lib) types;
in
{
  options.nxr.discoveryInputs = lib.mkOption {
    type = types.listOf types.str;
    default = [ ];
    example = [
      "Cargo.lock"
      "pnpm-lock.yaml"
    ];
    description = ''
      Extra flake-root-relative file paths whose contents are hashed into the
      nxr discovery cache key (cache schema v3).

      `*.nix` and `flake.lock` are already content-fingerprinted. Use this for
      non-Nix inputs that affect discovery metadata. Changing the path list in
      Nix invalidates via `.nix` content hashing; changing file contents
      invalidates on the next cache lookup without re-evaluating the list.

      Emitted on `nxr.<system>.discoveryInputs` so the CLI can hash them after
      task-document discovery (no separate eval on warm cache hits).
    '';
  };
}
