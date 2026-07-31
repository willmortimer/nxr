# Interactive TUI

Approved surface ([ADR-0173](https://github.com/willmortimer/nxr/blob/main/docs/adr/0173-operator-tui.md)):

| Command | Role |
|---|---|
| `nxr ui` | Browser of apps, tasks, and workspace scripts |
| `--output tui` | Ratatui DAG watch while a task runs |
| `nxr attach [RUN]` | Reopen a recorded watch |

## Browser

```bash
nxr ui
nxr --flake ./path/to/flake ui
```

Tabs: **Apps** / **Tasks** / **Scripts**. Arrow keys (or `j`/`k`) move; Tab or
`1`/`2`/`3` switches tabs; Enter runs:

- Apps → `nxr <app>`
- Tasks → `nxr task <name> --output tui`
- Scripts → `nxr script <name>`

Esc / `q` quit. Non-TTY: fail closed (exit 2).

## DAG watch

```bash
nxr task ci -j 8 --output tui
```

Shows node phases plus a selected log tail. Composes with `--log-dir`. On
non-TTY (or when the TUI cannot open), nxr falls back to `--output live` with a
stderr notice.

## Attach

```bash
nxr attach           # most recent attachable run
nxr attach <run-id>
```

Sidecars live under XDG state (`attach-runs/`) and optionally under `--log-dir`.
Fail closed if nothing exists. Requires a TTY.

## Multiplexers and clipboard

Under tmux/zellij, alternate-screen TUIs can fight the pane. Prefer a dedicated
pane, or use `live` / `grouped` output.

On task/app/script **failure**, nxr may emit **OSC 52** with a sanitized compact
summary (node names + exit/status). Disable with `NXR_OSC52=off`.

Demo GIFs in-repo: [`docs/demo/`](https://github.com/willmortimer/nxr/tree/main/docs/demo).
