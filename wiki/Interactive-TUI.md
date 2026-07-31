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

Tabs: **Apps** / **Tasks** / **Scripts**. Focus starts on the tab bar:

- ←/→ or `h`/`l` — switch tabs
- **Enter** or ↓ — open that tab's catalog (does not run yet)
- In catalog: ↑/↓ navigate; **Enter** runs the selection
- Esc / ← — back to tabs; `q` quits
- `1`/`2`/`3` jump to Apps/Tasks/Scripts

Launch mapping:

- Apps → `nxr <app>`
- Tasks → `nxr task <name> --output tui`
- Scripts → `nxr script <name>`

Non-TTY: fail closed (exit 2). Mouse capture is off so you can select/copy text
in the terminal (tmux/zellij scrollback still applies).

## DAG watch

```bash
nxr task ci -j 8 --output tui
```

Shows node phases plus a selected log tail. Composes with `--log-dir`. Falls
back to `--output live` when:

- stderr is not a TTY, or
- `TMUX` / `ZELLIJ` is set (alternate-screen conflict), unless `NXR_TUI=force`, or
- `NXR_TUI=off`

## Attach

```bash
nxr attach           # most recent TUI sidecar, else history summary
nxr attach <run-id>
```

Sidecars live under XDG state (`attach-runs/`) and optionally under `--log-dir`.
If no sidecar exists, attach synthesizes a summary from `nxr history`
(`hist-<epoch>-<target>`). Fail closed if neither exists. Requires a TTY.

## Multiplexers and clipboard

Under tmux/zellij, alternate-screen TUIs can fight the pane. Prefer a dedicated
pane, or use `live` / `grouped` output.

On task/app/script **failure**, nxr may emit **OSC 52** with a sanitized compact
summary (node names + exit/status). Disable with `NXR_OSC52=off`.

Demo GIFs in-repo: [`docs/demo/`](https://github.com/willmortimer/nxr/tree/main/docs/demo).
