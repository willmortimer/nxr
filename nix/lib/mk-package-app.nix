# Create a flake app that runs a binary from an existing package.
{
  pkgs,
  package,
  bin,
  description,
}:
let
  inherit (pkgs) lib;
  metadata = import ./metadata.nix { inherit lib; };
in
{
  type = "app";
  program = lib.getExe' package bin;
  meta = metadata.mkAppMeta { inherit description; };
}
