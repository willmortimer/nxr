{
  description = "nxr fixture: intentionally broken flake";

  outputs = _: {
    # Missing required flake output shape / invalid on purpose.
    this-is-not-valid = true;
  };
}
