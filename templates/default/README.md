# nxr consumer template

Minimal flake that imports `nxr.flakeModules.default` and defines a single
`hello` app via `nxr.apps`.

```bash
nix flake init -t github:willmortimer/nxr
nix run .#hello
```
