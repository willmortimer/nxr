# Operator patterns (decisions, watch, promote)

Short patterns that stay inside nxr’s contract: flake apps are leaves; tasks
coordinate; Cosign/Flux stay outside except **signing nxr’s own release blobs**.

## Decisions as infrastructure

Typed task `parameters` are not only env fill-ins. Use them for CI-scriptable
choices, and put **branching** in a thin wizard app when the graph shape must
change.

### Fail-closed `--set` (CI)

```bash
nxr task deploy --set env=staging --set reason="ticket-123"
# Non-interactive + required param without --set / NXR_PARAM_* / default → exit 2
```

Lookup order: `--set name=value` → `NXR_PARAM_<NAME>` → schema default → TTY
arrow-key / input prompt (stdin+stderr TTYs) → fail-closed.

Plans and events still list **parameter names only** (never values).

### Branching: wizard app → `nxr task …`

Prefer an interactive flake app that collects decisions, then execs a concrete
task graph, over a second DSL. See [`fixtures/deploy-wizard/`](../../fixtures/deploy-wizard/)
for a minimal wizard that branches to `deploy-staging` / `deploy-prod` tasks.

```bash
# apps.deploy-wizard prompts (TTY), then:
nxr deploy-wizard
# scripted / CI (fixture env overrides):
NXR_FIXTURE_WIZARD_ENV=staging nxr --flake fixtures/deploy-wizard deploy-wizard
# production path via tasks:
nxr task deploy-staging
# or
nxr task deploy-prod --set reason=…
```

The wizard owns branching; leaves remain `nix run`-compatible apps.

## One watch UX

One renderer ([ADR-0173](adr/0173-operator-tui.md)); do not ship competing UIs:

| Piece | Flag / behavior |
|---|---|
| Per-node log tee | `--log-dir PATH` → `PATH/<node>.stdout` / `.stderr` |
| Parallel status line | `--output live` (default for `-j > 1` on non-TUI) |
| DAG watch panel | `--output tui` (falls back to `live` when not a TTY, or under tmux/zellij unless `NXR_TUI=force`) |
| Re-open a run | `nxr attach [RUN]` (TUI sidecars first; else history summary) |
| Browse + run | `nxr ui` → focus tabs with ←/→, Enter opens catalog, Enter again runs |
| Follow running | TUI auto-follows newly started nodes; ↑/↓ pauses follow; `f` resumes |
| Wiki publish (this repo) | `nxr script publish-wiki` (optional; not on `release`) |
| Nix noise | `NXR_NIX_PROGRESS=auto\|builtin\|nom\|off` ([ADR-0172](adr/0172-nix-progress-formatter.md)) |

```bash
nxr --log-dir .nxr/logs/run-1 --output tui task ci -j 4
nxr attach
nxr ui
nxr script publish-wiki
```

Mouse selection/copy works in the TUI (mouse capture is off). Under tmux/zellij,
`--output tui` falls back to `live` by default; set `NXR_TUI=force` to keep the
alternate-screen watch.

### Multiplexer clipboard (OSC 52)

When a **task** or **app** run fails on a TTY, nxr emits an OSC 52 sequence so
terminals and multiplexers (tmux, zellij) can copy a compact failure digest to
the system clipboard. The payload lists failed node names and exit/status labels
only — never secret values (sanitized via the same path as terminal output).

```text
nxr failed
lint exit 1
gate cancelled
```

Disable with `NXR_OSC52=off` (also `0`, `false`, `no`). No emission in CI /
non-TTY contexts. Complements scrollback sizing in tmux/zellij; does not replace
the in-process TUI watch surface.
## Release / promote

For **this** repo: [RELEASE.md](RELEASE.md) — `nxr task ci` / `ci-linux`,
then `nxr task release` (dry-run) / `-- --execute` for signed `v*` tags;
GitHub Release + keyless Cosign on blobs.

For **consumer** promote (digest deploy, Flux, Cosign verify): keep that in the
deploy platform / Actions. nxr coordinates the build/test task graph and can
print digests via apps; it does not replace Cosign or Flux.
