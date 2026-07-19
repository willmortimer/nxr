# Helpers for defining standard flake apps that run against a source tree.
{ pkgs }:

let
  inherit (pkgs) lib;

  metadata = import ./metadata.nix { inherit lib; };
  mkApp = import ./mk-app.nix;
  mkPackageApp = import ./mk-package-app.nix;
in
{
  inherit mkApp mkPackageApp;
  mkScriptApp = mkApp;
  inherit (metadata) mkAppMeta mkAppsMetadataEntry schemaVersion;

  /*
    Create a flake app backed by writeShellApplication.

    The wrapper walks upward from $PWD looking for flake.nix + Cargo.toml so
    `nix run .#fmt` works from nested directories (same model nxr will use).
  */
  mkRepoApp =
    {
      name,
      description,
      runtimeInputs ? [ ],
      text,
    }:
    let
      drv = pkgs.writeShellApplication {
        inherit name runtimeInputs;
        text = ''
          root="$PWD"
          while [[ "$root" != "/" ]]; do
            if [[ -f "$root/flake.nix" && -f "$root/Cargo.toml" ]]; then
              cd "$root"
              break
            fi
            root="$(dirname "$root")"
          done
          if [[ ! -f Cargo.toml ]]; then
            echo "error: could not find nxr repository root (Cargo.toml + flake.nix)" >&2
            exit 1
          fi
          ${text}
        '';
      };
    in
    {
      type = "app";
      program = "${drv}/bin/${name}";
      meta = metadata.mkAppMeta { inherit description; };
    };

  /*
    Create a simple fixture flake app (no repo-root discovery).
  */
  mkSimpleApp =
    {
      name,
      description,
      runtimeInputs ? [ ],
      text,
    }:
    mkApp {
      inherit pkgs name description runtimeInputs text;
    };
}
