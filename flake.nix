{
  description = "nxr — ergonomic runner for Nix flake apps";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # nixos-unstable (26.11+) no longer evaluates x86_64-darwin; pin the last
    # branch that still builds Intel macOS until upstream restores support.
    nixpkgsIntelDarwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nixpkgs, nixpkgsIntelDarwin, ... }:
    let
      pkgsFor = system:
        import (if system == "x86_64-darwin" then nixpkgsIntelDarwin else nixpkgs) {
          inherit system;
        };
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = let
          metadata = import ./nix/lib/metadata.nix { lib = nixpkgs.lib; };
          mkApp = import ./nix/lib/mk-app.nix;
          mkPackageApp = import ./nix/lib/mk-package-app.nix;
        in {
          inherit mkApp mkPackageApp;
          mkScriptApp = mkApp;
          inherit metadata;
        };

        flakeModules.default = import ./nix/modules/flake-parts.nix;

        overlays.default = import ./nix/overlays/default.nix;

        templates.default = {
          path = ./templates/default;
          description = "Minimal nxr consumer flake using flake-parts";
        };
      };

      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      perSystem =
        {
          self',
          lib,
          system,
          ...
        }:
        let
          pkgs = pkgsFor system;

          nxrLib = import ./nix/lib { inherit pkgs; };

          src = import ./nix/lib/workspace-src.nix {
            inherit lib;
            root = ./.;
          };

          nxr = pkgs.callPackage ./nix/packages/nxr.nix { inherit src; };

          qualityChecks = import ./nix/checks/quality.nix {
            inherit pkgs src;
          };

          nxrApp = {
            type = "app";
            program = "${nxr}/bin/nxr";
            meta.description = "Run nxr";
          };

          rustDevInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            cargo-nextest
            cargo-deny
            pkg-config
          ];
        in
        {
          packages = {
            inherit nxr;
            default = nxr;
          };

          apps = {
            nxr = nxrApp;
            default = nxrApp;

            fmt = nxrLib.mkRepoApp {
              name = "nxr-fmt";
              description = "Format the Rust workspace (pass --check for CI)";
              runtimeInputs = [
                pkgs.cargo
                pkgs.rustfmt
              ];
              text = ''
                exec cargo fmt --all "$@"
              '';
            };

            lint = nxrLib.mkRepoApp {
              name = "nxr-lint";
              description = "Run Clippy on the workspace";
              runtimeInputs = [
                pkgs.cargo
                pkgs.clippy
                pkgs.rustc
              ];
              text = ''
                exec cargo clippy --workspace --all-targets -- -D warnings "$@"
              '';
            };

            test = nxrLib.mkRepoApp {
              name = "nxr-test";
              description = "Run the Rust test suite";
              runtimeInputs = [
                pkgs.cargo
                pkgs.cargo-nextest
                pkgs.rustc
              ];
              text = ''
                exec cargo nextest run --workspace "$@"
              '';
            };

            deny = nxrLib.mkRepoApp {
              name = "nxr-deny";
              description = "Run cargo-deny (advisories, licenses, bans)";
              runtimeInputs = [
                pkgs.cargo
                pkgs.cargo-deny
              ];
              text = ''
                exec cargo deny check "$@"
              '';
            };
          };

          checks = {
            inherit nxr;
          } // qualityChecks;

          formatter = pkgs.nixpkgs-fmt;

          devShells.default = pkgs.mkShell {
            packages = rustDevInputs ++ [
              self'.packages.nxr
            ];
            # Surface package-installed completions to direnv / nix develop.
            # Interactive zsh still needs shell/direnv-zsh-hook.zsh (see .envrc).
            shellHook = ''
              export XDG_DATA_DIRS="${self'.packages.nxr}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
              export FPATH="${self'.packages.nxr}/share/zsh/site-functions''${FPATH:+:$FPATH}"
            '';
          };
        };
    };
}
