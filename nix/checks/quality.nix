# Hermetic derivations for `checks.<system>.*` (invoked via `nix flake check`).
{
  pkgs,
  src,
}:
let
  inherit (pkgs) rustPlatform;

  advisoryDb = pkgs.fetchgit {
    url = "https://github.com/RustSec/advisory-db";
    rev = "b5fc89b8be99e96f79194d8a6f11e9b4143b99f0";
    hash = "sha256-BOqD2VjHKAjJB8uAltrYfjhci1JoiusWsHryUTup9FM=";
    leaveDotGit = true;
  };

  denyCheckConfig = pkgs.writeText "deny-ci.toml.in" ''
    [graph]
    all-features = true

    [advisories]
    yanked = "deny"
    db-path = "@DB@"
    db-urls = ["https://github.com/RustSec/advisory-db"]

    [licenses]
    allow = ["MIT", "Apache-2.0", "Unicode-3.0", "MPL-2.0", "ISC", "CC0-1.0"]
    confidence-threshold = 0.8

    [bans]
    multiple-versions = "warn"
    wildcards = "allow"

    [sources]
    unknown-registry = "deny"
    unknown-git = "deny"
    allow-registry = ["https://github.com/rust-lang/crates.io-index"]
  '';

  mkCargoCheck =
    {
      name,
      extraNativeBuildInputs ? [ ],
      command,
    }:
    rustPlatform.buildRustPackage {
      pname = "nxr-check-${name}";
      version = "2.1.0";

      inherit src;

      cargoLock.lockFile = "${src}/Cargo.lock";

      cargoBuildFlags = [
        "-p"
        "nxr-cli"
      ];

      nativeBuildInputs = extraNativeBuildInputs;

      doCheck = false;

      buildPhase = command;

      installPhase = "runHook preInstall; touch $out";
    };
in
{
  fmt = mkCargoCheck {
    name = "fmt";
    extraNativeBuildInputs = with pkgs; [ rustfmt ];
    command = "cargo fmt --all -- --check";
  };

  clippy = mkCargoCheck {
    name = "clippy";
    extraNativeBuildInputs = with pkgs; [ clippy ];
    command = "cargo clippy --workspace --all-targets -- -D warnings";
  };

  test = mkCargoCheck {
    name = "test";
    extraNativeBuildInputs = with pkgs; [
      cargo-nextest
      nix
    ];
    command = ''
      export NIX_CONFIG="experimental-features = nix-command flakes"
      cargo nextest run --workspace
    '';
  };

  deny = mkCargoCheck {
    name = "deny";
    extraNativeBuildInputs = with pkgs; [
      cargo-deny
      git
    ];
    command = ''
      mkdir -p "$TMPDIR/advisory-dbs/advisory-db-3157b0e258782691"
      cp -R ${advisoryDb}/. "$TMPDIR/advisory-dbs/advisory-db-3157b0e258782691/"
      chmod -R u+w "$TMPDIR/advisory-dbs/advisory-db-3157b0e258782691"
      (
        cd "$TMPDIR/advisory-dbs/advisory-db-3157b0e258782691"
        if [ ! -d .git ]; then
          git init -q
          git add -A
          git -c user.email=nxr@localhost -c user.name=nxr commit -qm advisory-db
        fi
      )
      sed "s|@DB@|$TMPDIR/advisory-dbs|" ${denyCheckConfig} > deny-ci.toml
      cargo deny --offline --config deny-ci.toml check
    '';
  };
}
