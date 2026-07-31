# Release process

Releases are driven by [`.github/workflows/release.yml`](../.github/workflows/release.yml).
A **minimal** quality gate runs on every push to `main` and on PRs via
[`ci.yml`](../.github/workflows/ci.yml) (single `ubuntu-latest` job, Determinate
Nix **v3.21.8**). Support-floor, Lix, and Darwin coverage runs on `v*` tags via
[`compat.yml`](../.github/workflows/compat.yml). The release workflow builds and
publishes artifacts (portable + Nix-package), checksums, SBOMs, and **keyless
Cosign** signatures for release blobs.

## Tag from the flake (preferred)

**Fail-closed:** `nxr task release -- --execute` (or `nix run .#release -- --execute`)
refuses to tag unless both local CI gate stamps exist for `HEAD`:

```bash
nxr task ci              # host: fmt → lint → test → deny → cli-ref (+ stamps host)
nxr task ci-linux        # Linux OS parity (+ stamps linux)
# Or one graph: nxr task release   # dependsOn [ci, ci-linux], then dry-run
```

Then, with `CHANGELOG.md` having a `## [X.Y.Z]` section matching `Cargo.toml`:

```bash
nxr task release                    # runs ci + ci-linux, then release dry-run
nxr task release -- --execute       # create signed tag + push (prompts unless --yes)
# Escape hatch: nix run .#release -- --execute --yes
```

Break-glass only: `--skip-gates` (loud warning). Not for normal releases.

Equivalent manual commands (what the app prints):

```bash
git tag -s "vX.Y.Z" -m "nxr X.Y.Z"
git push origin "refs/tags/vX.Y.Z"
```

The tag must match the flake package version. Pushing `v*` starts `release.yml`
and `compat.yml`.

## Triggers

| Event | Behavior |
|---|---|
| Push / PR to `main` | Minimal `ci.yml` quality gate |
| Push tag `v*` | `compat.yml` matrix + release build, Cosign, checksums, SBOM, GitHub Release |
| `workflow_dispatch` (release) | Same build steps; uploads workflow artifacts. Skips GitHub Release unless **dry_run** is unchecked |
| `workflow_dispatch` (compat) | Re-run the compatibility matrix without tagging |

Use **Actions → release → Run workflow** with **dry_run** enabled (default) to validate the pipeline without creating a release.

## Artifacts

For each supported flake system the workflow publishes **two** tarball classes:

| File | Contents |
|---|---|
| `nxr-<version>-<system>-nix-package.tar.gz` | Nix package layout (`bin/nxr`, man pages, shell completions, `share/nxr/shell/`, plus `README.txt`) — **not** a portable standalone binary |
| `nxr-<version>-<system>-portable.tar.gz` | Cargo release `bin/nxr` only (plus `README.txt`) — **portable**; no `/nix/store` linker dependencies |
| `SHA256SUMS` | `sha256sum` lines for every tarball |
| `nxr-cargo.cdx.json` | CycloneDX SBOM for the `nxr` CLI binary (`cargo-cyclonedx --describe binaries`) |
| `nxr-syft.cdx.json` | CycloneDX SBOM from the built Nix package (`syft dir:result`) |
| `*.sigstore.json` | Keyless Cosign Sigstore bundle per signed blob (Cosign v3) |

### Nix-package archives are not portable

Release archives ship the Nix package layout for inspection and asset reuse. The
`nxr` binary is a normal Nix build product: it needs its `/nix/store` runtime
closure, so extracting the tarball alone is not enough to execute `bin/nxr`
even when `nix` is on `PATH`. Each archive includes a `README.txt` that states
this explicitly. The release smoke job compares the archived binary to a fresh
`nix build` and runs fixture checks through the **store-backed** result path.

Prefer installing from the flake when you want a runnable binary with man pages
and completions:

```bash
nix profile install github:willmortimer/nxr#packages.x86_64-linux.nxr
# or
nix build github:willmortimer/nxr#packages.x86_64-linux.nxr
./result/bin/nxr --version
```

### Portable archives (ADR-0141)

Portable archives are built with `cargo build -p nxr-cli --release` per target.
The workflow asserts that `ldd` (Linux) or `otool -L` (macOS) reports no
`/nix/store` paths for the packaged binary. After extraction, `bin/nxr` runs
directly without materializing a Nix closure for the CLI itself. **Nix must
still be on `PATH`** — `nxr` shells out to `nix` for flake evaluation and app
execution.

Portable archives do **not** include man pages, shell completions, or
`share/nxr/shell/` assets. Use the matching `*-nix-package.tar.gz` or a flake
install when you need those files.

Systems match the root flake outputs:

- `aarch64-darwin`
- `x86_64-darwin`
- `aarch64-linux`
- `x86_64-linux`

Linux `x86_64` builds on `ubuntu-latest`; Linux `aarch64` builds on `ubuntu-24.04-arm` (native). Darwin archives build on `macos-latest` (Nix may cross-compile when the runner architecture differs; portable Darwin `x86_64` uses a cargo cross-target on Apple Silicon runners).

## Verification

After downloading a tarball:

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

**Cosign** (keyless, GitHub Actions OIDC; Cosign v3 bundle):

```bash
cosign verify-blob \
  --bundle nxr-….tar.gz.sigstore.json \
  --certificate-identity-regexp 'https://github.com/willmortimer/nxr/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  nxr-….tar.gz
```

**Nix-package layout** (needs Nix store closure to execute `bin/nxr`):

```bash
tar -xzf nxr-<version>-<system>-nix-package.tar.gz
test -x ./nxr-<version>-<system>-nix-package/bin/nxr
test -f ./nxr-<version>-<system>-nix-package/README.txt
ls ./nxr-<version>-<system>-nix-package/share/nxr/shell/
```

**Portable binary** (runs after extraction when Nix is on `PATH`):

```bash
tar -xzf nxr-<version>-<system>-portable.tar.gz
./nxr-<version>-<system>-portable/bin/nxr --version
# Linux: confirm no /nix/store linker deps
ldd ./nxr-<version>-<system>-portable/bin/nxr | grep -v '/nix/store' || true
```

The release workflow runs two smoke jobs:

1. **Nix-package smoke** — layout and labeling checks, `cmp` against a fresh
   `nix build`, then `--version`, completion generation, and fixture app/task
   invocations through the store-backed binary.
2. **Portable smoke** — extracts the Linux `x86_64` portable archive, asserts
   no `/nix/store` in `ldd` output, and runs `bin/nxr --version` **without**
   building the Nix package for that binary.

## Signing

| Layer | Mechanism |
|---|---|
| Git tag | Operator `git tag -s` (SSH/GPG); `.#release -- --execute` |
| Release blobs | Keyless Cosign in `release.yml` (`.sigstore.json` bundle per asset) |
| Broader provenance / promotion | Tracked in ADR-0409 / Phase 38 — not fully productized |

## Local dry run (artifacts)

From a flake checkout:

```bash
nix build .#packages.x86_64-linux.nxr -L
nix shell nixpkgs#cargo-cyclonedx nixpkgs#cargo nixpkgs#rustc --command \
  cargo cyclonedx -f json --manifest-path Cargo.toml --describe binaries
cp crates/nxr-cli/nxr_bin.cdx.json /tmp/nxr-cargo.cdx.json
find . -name '*_bin.cdx.json' -delete
nix shell nixpkgs#syft --command syft dir:result -o cyclonedx-json=/tmp/nxr-syft.cdx.json
```

Portable binary (no Nix build for the CLI artifact):

```bash
cargo build -p nxr-cli --release
ldd target/release/nxr | grep '/nix/store' && exit 1 || true   # Linux
./target/release/nxr --version
```
