# Build the `nxr` CLI from the Cargo workspace.
{
  lib,
  rustPlatform,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "nxr";
  version = "0.0.0";

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  cargoBuildFlags = [
    "-p"
    "nxr-cli"
  ];

  # Hermetic tests run via `apps.test` / CI, not during every package build.
  doCheck = false;

  meta = {
    description = "Ergonomic runner for Nix flake apps";
    homepage = "https://github.com/willmortimer/nxr";
    license = lib.licenses.mit;
    mainProgram = "nxr";
    platforms = lib.platforms.unix;
  };
}
