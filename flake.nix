{
  description = "nxr — ergonomic runner for Nix flake apps";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nixpkgs, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = {
          mkApp = import ./nix/lib/mk-app.nix;
          metadata = import ./nix/lib/metadata.nix { lib = nixpkgs.lib; };
        };

        flakeModules.default = import ./nix/modules/flake-parts.nix;
      };

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];

      perSystem =
        {
          pkgs,
          self',
          lib,
          ...
        }:
        let
          nxrLib = import ./nix/lib { inherit pkgs; };

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./deny.toml
              ./crates
              ./xtask
            ];
          };

          nxr = pkgs.callPackage ./nix/packages/nxr.nix { inherit src; };

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
            default = {
              type = "app";
              program = "${nxr}/bin/nxr";
              meta.description = "Run nxr";
            };

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
          };

          devShells.default = pkgs.mkShell {
            packages = rustDevInputs ++ [
              self'.packages.nxr
            ];
          };
        };
    };
}
