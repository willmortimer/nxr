# CI and Release

The product claim: **one inspectable graph**, locally and on Actions.

```bash
nxr task ci          # host quality DAG
nxr task ci-linux    # Linux OS parity (OrbStack/Docker when needed)
nxr task release     # dependsOn [ci, ci-linux], then tag helper
nxr ci plan --json   # provider-neutral export
```

In **this** repository, GitHub Actions dogfoods the packaged `$NXR task ci`
path. Details:
[CONTRIBUTING.md](https://github.com/willmortimer/nxr/blob/main/docs/CONTRIBUTING.md),
[RELEASE.md](https://github.com/willmortimer/nxr/blob/main/docs/RELEASE.md).

## Affected / selectors

```bash
nxr affected --base origin/main --json
nxr task --affected --path shared/lib.txt
```

## Outputs for CI logs

Prefer non-interactive modes in pipelines: `live`, `grouped`, `failures`,
`summary`, or `--events jsonl`. Reserve `--output tui` / `nxr ui` for TTYs.
