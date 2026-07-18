# Build the `nxr` CLI from the Cargo workspace.
{
  lib,
  rustPlatform,
  installShellFiles,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "nxr";
  version = "0.1.0";

  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  cargoBuildFlags = [
    "-p"
    "nxr-cli"
  ];

  nativeBuildInputs = [ installShellFiles ];

  # Hermetic tests run via `apps.test` / CI, not during every package build.
  doCheck = false;

  postInstall = ''
    installShellCompletion --cmd nxr \
      --bash <($out/bin/nxr completion bash) \
      --zsh  <($out/bin/nxr completion zsh) \
      --fish <($out/bin/nxr completion fish)
  '';

  meta = {
    description = "Ergonomic runner for Nix flake apps";
    homepage = "https://github.com/willmortimer/nxr";
    license = lib.licenses.mit;
    mainProgram = "nxr";
    platforms = lib.platforms.unix;
  };
}
