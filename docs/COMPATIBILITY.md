# Compatibility

## Supported platforms (V1)

Initial release matrix:

| System | Architecture | CI | Notes |
|---|---|---|---|
| `aarch64-darwin` | Apple Silicon macOS | `macos-latest` | Primary developer platform |
| `x86_64-linux` | Linux (amd64) | `ubuntu-latest` | Primary CI platform |

Other Unix targets may build from source but are not part of the V1 compatibility guarantee until listed here.

## Nix

`nxr` delegates execution to the Nix CLI. A working `nix` on `PATH` is required.

Minimum supported Nix versions are determined through capability detection rather than a hard-coded floor. See [ARCHITECTURE.md](ARCHITECTURE.md) §9 and ADR-0018 in [adr/README.md](adr/README.md).

## Escape hatch

Every V1 operation has an equivalent native form:

```bash
nix run .#<app> -- [args...]
```

Projects remain operable without `nxr` installed. See [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md).

## Reporting gaps

Open an issue with:

- `uname -m` and OS version;
- `nix --version`;
- the `nxr` command and flake reference;
- `nxr plan <app> --json` when execution fails.
