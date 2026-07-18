{
  description = "nxr fixture: intentionally broken flake";

  outputs =
    _: {
      # Forces `nix flake show` to fail so nxr can exercise evaluation diagnostics.
      apps = builtins.throw "intentionally broken flake (fixture)";
    };
}
