# Create a standard flake app backed by writeShellApplication.
{
  pkgs,
  name,
  description,
  category ? null,
  runtimeInputs ? [ ],
  text,
}:
let
  inherit (pkgs) lib;
  metadata = import ./metadata.nix { inherit lib; };

  drv = pkgs.writeShellApplication {
    inherit name runtimeInputs text;
  };
in
{
  type = "app";
  program = "${drv}/bin/${name}";
  meta = metadata.mkAppMeta (
    { inherit description; } // lib.optionalAttrs (category != null) { inherit category; }
  );
}
