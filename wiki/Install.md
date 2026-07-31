# Install

## Profile or shell

```bash
nix profile install github:willmortimer/nxr#nxr
# or one-shot:
nix shell github:willmortimer/nxr#nxr
```

Pre-built release tarballs (Nix-package layout vs portable cargo binary):
[docs/RELEASE.md](https://github.com/willmortimer/nxr/blob/main/docs/RELEASE.md).

## Flake-parts

```nix
imports = [ inputs.nxr.flakeModules.default ];

perSystem.nxr = {
  shellIntegration.enable = true;
  # optional: shellIntegration.devShells = [ "default" "backend" ];
  tasks.ci = { app = "ci"; };
};
```

The module can export `exportedSchemas.nxr` (disable with
`nxr.schema.enable = false`). Runtime task documents remain `nxr.<system>` via
ordinary `nix eval` on upstream Nix and Lix.

More: [DEV_ENV_INTEGRATION.md](https://github.com/willmortimer/nxr/blob/main/docs/DEV_ENV_INTEGRATION.md).

## Smoke check

```bash
nxr --version
nxr list
nxr doctor
```
