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
      imports = [
        ./nix/modules/root-tasks-only.nix
      ];

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

        exportedSchemas.nxr = import ./nix/schemas/nxr.nix;

        flakeModules.default = import ./nix/modules/flake-parts.nix;

        # Reusable Home Manager module only — do not ship homeConfigurations.
        homeManagerModules.default = import ./nix/modules/home-manager.nix;

        overlays.default = import ./nix/overlays/default.nix;

        templates.default = {
          path = ./templates/default;
          description = "Minimal nxr consumer flake using flake-parts";
        };
        templates.rust = {
          path = ./templates/rust;
          description = "Rust project with fmt/lint/test apps and a ci task graph";
        };
        templates.node = {
          path = ./templates/node;
          description = "Node.js project with install/lint/test apps and a ci task graph";
        };
        templates.mixed = {
          path = ./templates/mixed;
          description = "Rust + Node project with separate apps and a combined ci task";
        };
        templates.monorepo = {
          path = ./templates/monorepo;
          description = "Namespaced monorepo with categories, tasks, and nxr.projects.json";
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

          flakeSchemaCheck = import ./nix/checks/flake-schema.nix {
            inherit pkgs;
          };

          flakePartsV2FieldsCheck = import ./nix/checks/flake-parts-v2-fields.nix {
            inherit pkgs;
          };

          nxrMetadataDocumentCheck = import ./nix/checks/nxr-metadata-document.nix {
            inherit pkgs;
          };

          workspaceSrcIncludesCheck = import ./nix/checks/workspace-src-includes.nix {
            inherit pkgs src;
          };

          # Eval x86_64-linux NixOS assertions on every check host (incl. Darwin).
          configurationsFixtureCheck = import ./nix/checks/configurations-fixture.nix {
            inherit pkgs;
            nixpkgs = if system == "x86_64-darwin" then nixpkgsIntelDarwin else nixpkgs;
          };

          nxrApp = {
            type = "app";
            program = "${nxr}/bin/nxr";
            meta.description = "Run nxr";
          };

          # Host env that changes CLI/nextest outcomes vs clean GHA runners.
          hermeticRunnerEnv = ''
            unset NXR_DEV_SHELL || true
            export GIT_CONFIG_GLOBAL=/dev/null
            export GIT_CONFIG_SYSTEM=/dev/null
          '';

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
          nxr.tasks = {
            fmt = {
              description = "Format Rust sources";
              app = "fmt";
            };

            fmt-check = {
              description = "Verify Rust formatting (CI)";
              app = "fmt-check";
              hidden = true;
            };

            lint = {
              description = "Run Clippy on the workspace";
              app = "lint";
              dependsOn = [ "fmt-check" ];
            };

            test = {
              description = "Run the Rust test suite";
              app = "test";
              dependsOn = [ "lint" ];
            };

            deny = {
              description = "Run cargo-deny";
              app = "deny";
              dependsOn = [ "fmt-check" ];
            };

            ci = {
              description = "CI quality gate";
              app = "ok";
              category = "validation";
              dependsOn = [
                "test"
                "deny"
              ];
              aliases = [ "check" ];
            };

            release = {
              description = "Host CI gate then prepare/create the v* release tag";
              app = "release";
              category = "validation";
              dependsOn = [ "ci" ];
            };
          };

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
                ${hermeticRunnerEnv}
                exec cargo fmt --all "$@"
              '';
            };

            fmt-check = nxrLib.mkRepoApp {
              name = "nxr-fmt-check";
              description = "Verify Rust formatting (CI)";
              runtimeInputs = [
                pkgs.cargo
                pkgs.rustfmt
              ];
              text = ''
                ${hermeticRunnerEnv}
                exec cargo fmt --all -- --check "$@"
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
                ${hermeticRunnerEnv}
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
                pkgs.git
              ];
              text = ''
                ${hermeticRunnerEnv}
                # Use the ambient flakes-capable `nix` (Determinate on GHA /
                # local). Do not pin pkgs.nix here — that changes discovery
                # capability negotiation vs the Actions runner.
                # --no-fail-fast: watch ITs under parallel cancel leave orphans
                # that poison sibling nix-call budgets; report the full set.
                exec cargo nextest run --workspace --no-fail-fast "$@"
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
                ${hermeticRunnerEnv}
                exec cargo deny check "$@"
              '';
            };

            # Single local ≡ GHA dogfood entrypoint (packaged nxr + task ci graph).
            # Do not add pkgs.nix: nested `nix run .#test` must see the same
            # ambient flakes-capable Nix as Actions (Determinate). Pinning
            # nixpkgs' nix flips discovery to flake-show and fails call-budget ITs.
            ci-gate = nxrLib.mkRepoApp {
              name = "nxr-ci-gate";
              description = "Same quality dogfood as GitHub Actions (nxr task ci)";
              runtimeInputs = [
                nxr
                pkgs.git
              ];
              text = ''
                ${hermeticRunnerEnv}
                nxr ci plan --json >/dev/null
                nxr task ci --dry-run
                nxr task ci "$@"
                # Linux dogfood runs this app too; only the host entrypoint stamps "host".
                if [[ "''${NXR_CI_LINUX:-}" != "1" ]]; then
                  exec ./scripts/release-gates.sh stamp host
                fi
              '';
            };

            # Pre-push bar on Darwin: same gate inside Linux + Determinate
            # (OrbStack/Docker). Host `ci-gate` cannot catch Linux process ITs.
            ci-gate-linux = nxrLib.mkRepoApp {
              name = "nxr-ci-gate-linux";
              description = "Run ci-gate on Linux via OrbStack/Docker (GHA parity)";
              runtimeInputs = [
                pkgs.bash
                pkgs.coreutils
              ];
              text = ''
                # writeShellApplication resets PATH; recover host Docker/OrbStack.
                export PATH="/usr/local/bin:/opt/homebrew/bin:/bin:/usr/bin:$PATH"
                exec ./scripts/ci-gate-linux.sh "$@"
              '';
            };

            # Release tag helper. Do not clear git signing config — `git tag -s`
            # needs the operator's SSH/GPG key (1Password agent, etc.).
            release = nxrLib.mkRepoApp {
              name = "nxr-release";
              description = "Verify version sync and prepare/create signed v* tag";
              runtimeInputs = [
                pkgs.bash
                pkgs.coreutils
                pkgs.git
                pkgs.gnugrep
                pkgs.gnused
                pkgs.gawk
              ];
              text = ''
                unset NXR_DEV_SHELL || true
                exec ./scripts/release.sh "$@"
              '';
            };

            ok = nxrLib.mkRepoApp {
              name = "nxr-ok";
              description = "No-op success (task graph sink)";
              text = ''
                exit 0
              '';
            };
          };

          checks = {
            inherit nxr;
            flake-schema = flakeSchemaCheck;
            flake-parts-v2-fields = flakePartsV2FieldsCheck;
            nxr-metadata-document = nxrMetadataDocumentCheck;
            workspace-src-includes = workspaceSrcIncludesCheck;
            configurations-fixture = configurationsFixtureCheck;
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
