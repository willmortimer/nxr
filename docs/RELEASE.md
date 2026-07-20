# Release process

Releases are driven by [`.github/workflows/release.yml`](../.github/workflows/release.yml). Quality gates run on every push to `main` via [`ci.yml`](../.github/workflows/ci.yml); the release workflow builds and publishes artifacts only.

## Triggers

| Event | Behavior |
|---|---|
| Push tag `v*` | Build all targets, generate checksums and SBOM, publish a GitHub Release |
| `workflow_dispatch` | Same build steps; uploads workflow artifacts. Skips GitHub Release unless **dry_run** is unchecked |

Use **Actions → release → Run workflow** with **dry_run** enabled (default) to validate the pipeline without creating a release.

## Artifacts

For each supported flake system the workflow builds `.#packages.<system>.nxr`,
stages the Nix package layout (`bin/nxr`, man pages, shell completions, and
`share/nxr/shell/`), and attaches:

| File | Contents |
|---|---|
| `nxr-<version>-<system>.tar.gz` | `nxr-<version>-<system>/` with `bin/`, `share/man/`, completion scripts, and `share/nxr/shell/` |
| `SHA256SUMS` | `sha256sum` lines for every tarball |
| `nxr-cargo.cdx.json` | CycloneDX SBOM for the `nxr` CLI binary (`cargo-cyclonedx --describe binaries`) |
| `nxr-syft.cdx.json` | CycloneDX SBOM from the built Nix package (`syft dir:result`) |

Release archives ship the Nix package layout (`bin/nxr`, man, completions,
shell integration) for inspection and asset reuse. The `nxr` ELF is a normal
Nix build product: it needs its `/nix/store` runtime closure, so extracting the
tarball alone is not enough to execute `bin/nxr` even when `nix` is on `PATH`.

Prefer installing from the flake when you want a runnable binary:

```bash
nix profile install github:willmortimer/nxr#packages.x86_64-linux.nxr
# or
nix build github:willmortimer/nxr#packages.x86_64-linux.nxr
./result/bin/nxr --version
```

Systems match the root flake outputs:

- `aarch64-darwin`
- `x86_64-darwin`
- `aarch64-linux`
- `x86_64-linux`

Linux `x86_64` builds on `ubuntu-latest`; Linux `aarch64` builds on `ubuntu-24.04-arm` (native). Darwin archives build on `macos-latest` (Nix may cross-compile when the runner architecture differs).

## Verification

After downloading a tarball:

```bash
sha256sum -c SHA256SUMS --ignore-missing
tar -xzf nxr-<version>-<system>.tar.gz
# Layout check (ELF needs a Nix store closure to execute):
test -x ./nxr-<version>-<system>/bin/nxr
ls ./nxr-<version>-<system>/share/nxr/shell/
```

The release workflow extract-and-smoke job verifies archive layout, asserts the
tag/package version match on every matrix build, `cmp`s the uploaded `bin/nxr`
against a fresh `nix build` of the tagged flake (same content-addressed output),
then runs `--version`, completion generation, and fixture app/task invocations
through that store-backed binary.

## Signing gap

Release artifacts are **not** cryptographically signed today. Checksums and SBOMs support integrity and supply-chain visibility but do not replace detached signatures or provenance attestations. Code signing, Sigstore/cosign, and SLSA provenance are tracked for a later release-engineering pass (see [adr/README.md](adr/README.md), ADR-0409).

## Local dry run

From a flake checkout:

```bash
nix build .#packages.x86_64-linux.nxr -L
nix shell nixpkgs#cargo-cyclonedx nixpkgs#cargo nixpkgs#rustc --command \
  cargo cyclonedx -f json --manifest-path Cargo.toml --describe binaries
cp crates/nxr-cli/nxr_bin.cdx.json /tmp/nxr-cargo.cdx.json
find . -name '*_bin.cdx.json' -delete
nix shell nixpkgs#syft --command syft dir:result -o cyclonedx-json=/tmp/nxr-syft.cdx.json
```
