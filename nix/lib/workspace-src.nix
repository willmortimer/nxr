# Filtered workspace source for hermetic Cargo builds and checks.
{
  lib,
  root,
}:
lib.fileset.toSource {
  inherit root;
  fileset = lib.fileset.unions [
    (root + "/Cargo.toml")
    (root + "/Cargo.lock")
    (root + "/deny.toml")
    (root + "/crates")
    (root + "/fixtures")
    (root + "/schemas")
    (root + "/shell")
    (root + "/tests")
    (root + "/xtask")
  ];
}
