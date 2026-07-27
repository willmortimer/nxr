# Filtered workspace source for hermetic Cargo builds and checks.
#
# Any path reached by `include_str!` / `include_bytes!` from crates/, or by
# package `postInstall`, MUST appear in this fileset. The
# `workspace-src-includes` check walks those macros against the filtered tree
# so omitting a directory fails CI before release.
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
    (root + "/templates")
    (root + "/tests")
    (root + "/xtask")
  ];
}
