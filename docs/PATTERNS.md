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
task graph, over a second DSL:

```bash
# apps.deploy-wizard prompts (TTY), then:
nxr task deploy-staging
# or
nxr task deploy-prod --set reason=…
```

The wizard owns branching; leaves remain `nix run`-compatible apps.

## One watch UX

Compose later; do not ship three UIs. Today:

| Piece | Flag / behavior |
|---|---|
| Per-node log tee | `--log-dir PATH` → `PATH/<node>.stdout` / `.stderr` |
| Parallel status | `--output live` (default for `-j > 1`) compact stderr status line |
| Nix noise | `NXR_NIX_PROGRESS=auto\|builtin\|nom\|off` ([ADR-0172](adr/0172-nix-progress-formatter.md)) |

```bash
nxr --log-dir .nxr/logs/run-1 --output live task ci -j 4
```

Full TUI / multiplexer panes are later (roadmap Phase 35); keep one renderer.

## Release / promote

For **this** repo: [RELEASE.md](RELEASE.md) — `nxr task ci` / `ci-linux`,
then `nxr task release` (dry-run) / `-- --execute` for signed `v*` tags;
GitHub Release + keyless Cosign on blobs.

For **consumer** promote (digest deploy, Flux, Cosign verify): keep that in the
deploy platform / Actions. nxr coordinates the build/test task graph and can
print digests via apps; it does not replace Cosign or Flux.
