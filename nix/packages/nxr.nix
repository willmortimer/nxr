# Build the `nxr` CLI from the Cargo workspace.
{
  lib,
  rustPlatform,
  installShellFiles,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "nxr";
  version = "1.0.0";

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
    $out/bin/nxr __manpage > nxr.1
    installManPage nxr.1
  '';

  meta = {
    description = "Ergonomic runner for Nix flake apps";
    homepage = "https://github.com/willmortimer/nxr";
    license = lib.licenses.mit;
    mainProgram = "nxr";
    platforms = lib.platforms.unix;
  };
}
