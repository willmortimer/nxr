# CLI Contract

## 1. Command grammar

Canonical grammar:

```text
nxr [GLOBAL_OPTIONS] [COMMAND_OR_APP] [COMMAND_OPTIONS_OR_APP_ARGS]
```

Primary forms:

```bash
nxr
nxr <app> [args...]
nxr run <app> [--] [args...]
nxr list [apps|checks|packages|shells|tasks]
nxr build [package]
nxr check [check]
nxr shell [devShell]
nxr select
nxr plan <app-or-task>
nxr doctor [app]
nxr doctor determinate [--all] [--refresh]
nxr explain <app|task> [args...]
nxr explain task <name> [args...]
nxr completion <shell>
nxr cache clear|status
nxr daemon start|stop|status
nxr inspect ...
nxr task <task> [args...]
nxr watch <app-or-task>
nxr watch app:<name>
nxr watch task:<name>
nxr ci plan [--json]
nxr graph <task>
nxr script <path-or-name> [--] [args...]
nxr attach [RUN]
nxr ui
```

V1 implements the app-oriented subset. V2 activates task-oriented commands. Flake output commands (`list` filters, `build`, `check`, `shell`) map to native Nix operations without inventing a second authority.

Reserved `script` ([ADR-0169](adr/0169-workspace-script-execution.md)): run a local
workspace script (exact path or `.nxr/scripts/<name>`). Does **not** participate
in bare `nxr <name>` resolution. Local checkout only. Optional `--context <name>`
applies schema v2 execution contexts (environment, secrets, confirm) like tasks.

Reserved `attach` / `ui` ([ADR-0173](adr/0173-operator-tui.md)):

- `nxr attach [RUN]` — reopen the TUI watch for a history/log-dir run (omit RUN
  for the most recent attachable run; fail closed if none).
- `nxr ui` — lazygit-style browser of apps, tasks, and workspace scripts; Enter
  runs the selection with `--output tui` (scripts via `nxr script`).

Neither participates in bare `nxr <name>` resolution.

## 2. Name resolution

For:

```bash
nxr test
```

resolution order is:

### V1

1. reserved top-level command;
2. app in `apps.<current-system>.test`;
3. error with suggestions.

### V2

1. reserved top-level command;
2. explicit alias;
3. task named `test`;
4. app named `test`;
5. ambiguity error if policy does not establish a winner.

Explicit forms always work:

```bash
nxr run test
nxr task test
```

Reserved command conflicts are resolved through `nxr run <name>`.

## 3. Flake selection

Supported forms:

```bash
nxr test
nxr --flake . test
nxr --flake ../project test
nxr --flake github:owner/project test
nxr github:owner/project#test
```

Rules:

- no `--flake`: discover a local flake upward from the invocation directory;
- local path: resolve relative to the invocation directory;
- remote reference: do not perform local root discovery for target resolution;
- an inline `flake#app` reference selects both flake and app;
- conflicting selectors are errors.

## 4. Global options

Stable V1 options:

```text
-f, --flake <REF>          Select flake reference
-C, --cwd <PATH>           Set child working directory
    --root                 Run child from flake root
-s, --select               Open interactive selector
-q, --quiet                Suppress non-error nxr messages
-v, --verbose              Increase runner diagnostics
    --plain                Disable decorative terminal output
    --json                 Emit JSON for data-returning commands
    --log-format <FORMAT>  human|plain|json
    --clean-env            Run with reduced inherited environment
    --keep-env <NAME>      Preserve variable in clean mode
    --set-env <K=V>        Set or replace a variable
    --unset-env <NAME>     Remove a variable
    --dry-run              Print plan without execution
    --no-color             Disable runner color
    --color <WHEN>         auto|always|never
    --nix <PATH>           Override Nix executable
    --refresh-discovery    Ignore nxr discovery cache
    --offline              Forward `--offline` to Nix (errors when unsupported)
    --accept-flake-config  Forward `--accept-flake-config` to Nix (errors when unsupported)
    --nix-option <KEY=VAL> Forward `--option KEY VAL` to Nix (repeatable)
    --nix-arg <ARG>        Forward arbitrary Nix argv fragments (repeatable)
-h, --help                 Show help
-V, --version              Show version
```

Deferred (not stable yet):

```text
```

Use `--nix-arg --refresh` to forward Nix's `--refresh` global when needed.

V2 / upcoming orchestration options:

```text
    --shell <NAME>         Execute through named dev shell
    --shell-mode <MODE>    smart|always|never (default smart)
-j, --jobs <N>             Maximum parallel task nodes
    --fail-fast            Cancel independent work after failure
    --keep-going           Continue independent work
    --output <MODE>        live|grouped|failures|summary|raw
    --events <FORMAT>      jsonl
    --log-dir <PATH>       Tee per-node stdout/stderr under PATH
    --watch                Watch and rerun/restart
    --debounce <DURATION>  Watch debounce
```

Task-only: `task --set NAME=VALUE` (repeatable) for typed parameters.
## 5. Argument forwarding

### 5.1 Direct form

```bash
nxr test --nocapture
```

After resolving `test` as an app, `--nocapture` belongs to the app.

### 5.2 Explicit separator

```bash
nxr test -- --nocapture
```

One separator is removed; subsequent arguments are forwarded exactly.

### 5.3 Reserved runner flags after app name

Runner options should normally appear before the app:

```bash
nxr --quiet test --nocapture
```

After the app name, arguments are treated as app arguments. This avoids stealing flags from the app.

Commands with their own parser use explicit command positions:

```bash
nxr doctor --clean-env test
nxr plan --json test
```

### 5.4 No shell evaluation

Input:

```bash
nxr command '$(rm -rf /)'
```

passes the literal argument to the app. `nxr` does not evaluate it.

## 6. Exit codes

Proposed runner exit codes:

```text
0   successful execution or query
1   child operation failed with generic status when exact status unavailable
2   CLI usage error
3   flake discovery or resolution error
4   Nix capability/version error
5   evaluation error
6   app/task not found
7   invalid nxr metadata
8   task graph planning error
9   process supervision error
10  interrupted before child status was available
```

When a single app exits normally, `nxr` should return the app's exit code whenever representable.

For V2 task graphs, the runner returns:

- `0` if all required nodes succeed;
- the first failed node's exit code when deterministic and representable;
- otherwise a documented orchestration failure code.

Signal termination should follow platform conventions.

## 7. Standard output and standard error

### Human mode

- child stdout remains stdout;
- child stderr remains stderr;
- normal runner status messages go to stderr so stdout can remain pipeable;
- `nxr list` writes its data to stdout.

### JSON mode

- JSON payload goes to stdout;
- diagnostics go to stderr;
- no decorative text appears on stdout.

### Event mode

JSON Lines events follow [`schemas/events-v1.schema.json`](../schemas/events-v1.schema.json)
(`type`-tagged `Event` objects, one per line). Stdout/stderr chunks carry a
`text` field plus optional `encoding`:

- absent or `utf8` — `text` is a UTF-8 string;
- `base64` — `text` is standard base64 of raw bytes (binary-safe round-trip).

Pipe readers never apply `from_utf8_lossy` at chunk boundaries; human multiplex
modes decode UTF-8 incrementally so multi-byte characters split across reads
are not corrupted.

### Output modes

```text
--output live       Prefix each line with [node] as chunks arrive
--output grouped    Buffer per node; flush on exit
--output failures   Buffer per node; emit only on nonzero exit
--output raw        Single-job foreground child inherits stdio (no pipe mux)
--output summary    Per-node status/duration table after the run
--output tui        Ratatui one-screen DAG watch (node table + log tail)
```

`--output raw` requires `-j 1` and cannot be combined with `--events`. It
bypasses line-oriented event conversion so binary and interactive child I/O
pass through unchanged. Multiplexed modes (`live` / `grouped` / `failures` /
`tui`) close caller stdin for supervised children.

`--output summary` prints a per-node status/duration table (shipped in 2.4).

`--output tui` ([ADR-0173](adr/0173-operator-tui.md)) renders a live DAG watch
from the task event stream and composes with `--log-dir`. When stderr/stdout
are not TTYs (CI), nxr **falls back to `live`** and prints a stderr notice.
`nxr attach` reuses the same renderer for a completed or in-progress run
recorded under history / `--log-dir`.

```bash
nxr --output tui --log-dir .nxr/logs/run task ci -j 4
nxr attach
nxr attach <run-id>
nxr ui
```
## 8. App listing contract

Human:

```text
Available apps for aarch64-darwin

  dev        Start local development services
  lint       Run static analysis
  test       Run the test suite
```

JSON:

```json
{
  "schema_version": 1,
  "flake": ".",
  "system": "aarch64-darwin",
  "apps": [
    {
      "name": "test",
      "attr_path": "apps.aarch64-darwin.test",
      "description": "Run the test suite",
      "default": false
    }
  ]
}
```

Ordering is stable and lexicographic unless explicit metadata defines display order.

`nxr list` without a kind lists apps and, when present, tasks. Optional kinds:

```bash
nxr list apps
nxr list checks
nxr list packages
nxr list shells
nxr list tasks
```

Filtered package/check/shell JSON uses:

```json
{
  "schema_version": 1,
  "flake": ".",
  "system": "aarch64-darwin",
  "kind": "packages",
  "outputs": [
    {
      "name": "nxr",
      "attr_path": "packages.aarch64-darwin.nxr",
      "description": "Ergonomic runner for Nix flake apps",
      "default": false
    }
  ]
}
```

## 8.1 Native flake output commands

These map directly to Nix; they do not redefine checks as tasks.

```bash
nxr build [name]     # nix build <flake>#packages.<system>.<name>  (or nix build <flake>)
nxr check [name]     # nix build <flake>#checks.<system>.<name>    (or nix flake check)
nxr shell [name]     # nix develop <flake>#<name>                  (or nix develop <flake>)
```

`--dry-run` prints the planned `nix` argv (JSON with `--json`). Missing named outputs exit `6` with suggestions.

## 9. Plan contract

```bash
nxr plan test --json
```

returns:

```json
{
  "schema_version": 1,
  "kind": "app",
  "flake": "/absolute/project/path",
  "system": "aarch64-darwin",
  "target": "test",
  "attr_path": "apps.aarch64-darwin.test",
  "invocation_directory": "/absolute/project/path/crates/api",
  "execution_directory": "/absolute/project/path/crates/api",
  "environment_policy": "inherit",
  "command": {
    "program": "nix",
    "arguments": [
      "run",
      "/absolute/project/path#test",
      "--"
    ]
  },
  "forwarded_arguments": []
}
```

Sensitive environment values must not be printed unless explicitly requested.

## 10. Doctor contract

Default doctor is static and non-destructive.

It may evaluate and resolve apps but does not execute them.

Execution checks that run apps require an explicit future flag (not shipped):

```bash
nxr doctor --execute-safe   # deferred
```

Clean-environment validation never executes apps; with a named app it may emit a dry-run plan only:

```bash
nxr doctor --clean-env test
nxr doctor --all
```

`doctor --all` adds non-destructive workspace findings: app description/naming quality, discovery cache status, invalidation key, and a structured `workspace` object in JSON (flake root, system, Nix capabilities, cache metadata).

`nxr doctor determinate` reports read-only Determinate Nix integration findings (distribution, effective experimental features, substituters and trusted keys with secrets redacted, lazy trees / parallel evaluation when detectable, Wasm experimental features when configured, CI environment detection, `determinate-nixd` presence, remote-builder heuristics with `--all`). On upstream Nix or Lix it emits a single informational `determinate.distribution.na` finding. Capability-cache warm hits reuse stored version/config probes (zero additional `nix --version` / `nix config show` calls). JSON output redacts token-like values from nixd status; secrets never appear in findings.

`nxr explain` prints how nxr would resolve and invoke an app or task: flake root, system, Nix executable/version/capabilities, discovery cache hit/miss and invalidation key, attr path, execution directory, environment policy, requested/active dev shell, exact Nix argv, task dependency path, shell-wrap skip reason, and scheduler skip reasons for dependent task nodes.

Apps may declare themselves unsafe for automatic execution.

Doctor findings have levels:

```text
info
warning
error
```

JSON findings contain stable codes.

## 11. Completion contract

```bash
nxr completion zsh
```

prints a shell script to stdout.

Dynamic completion must:

- complete reserved commands;
- discover current-flake apps;
- discover V2 tasks;
- display descriptions;
- avoid emitting diagnostics into completion output;
- cache briefly;
- invalidate on relevant file changes;
- time out gracefully when Nix evaluation is slow.

## 12. V2 task command contract

```bash
nxr task ci
nxr task ci --jobs 4
nxr task deploy --set env=staging --set reason=ticket-123
nxr --log-dir .nxr/logs/run --output live task ci -j 4
nxr --log-dir .nxr/logs/run --output tui task ci -j 4
nxr task --affected [--base REF | --working-tree | --all-changes REF | --path PATH…] [name…]
nxr plan --affected [--base REF | --working-tree | --all-changes REF | --path PATH…] [name…]
nxr graph ci
nxr watch dev
nxr watch app:dev
nxr watch task:ci
```

Trailing arguments after the task name are forwarded to the **root task app only**
(`argument_forwarding: root`). Dependency nodes always receive an empty argument
list. Richer per-node forwarding is deferred.

`task --set NAME=VALUE` (repeatable) supplies typed schema parameters (lookup:
`--set` → `NXR_PARAM_*` → default → TTY prompt → fail-closed). Unknown names are
errors. Global `--log-dir PATH` tees per-node stdout/stderr when stdio is piped.

`--affected` uses the same path sources and strict policy as `nxr affected`
(default strict). Without task names, roots are the union of all affected tasks;
with names, roots are the intersection (unaffected named tasks are skipped). An
empty affected set is a successful no-op (exit 0). `task --affected` conflicts
with `--watch`. Prefer `--path` on these commands (not positional paths) so task
names stay unambiguous.

Task selectors (2.8): `category:<name>` expands to listable tasks in that
category; `changed` is equivalent to `--affected` (requires `--base`,
`--working-tree`, `--all-changes`, or `--path`). Existing `app:` / `task:`
prefixes are unchanged. Selectors work on `nxr list`, `nxr task`, and `nxr plan`
where natural.

```bash
nxr list category:validation
nxr task category:validation
nxr plan changed --path shared/lib.txt
nxr ci plan --json
```

`nxr ci plan --json` emits [`ci-plan-v1`](../schemas/ci-plan-v1.schema.json):
flake/system, task roots, nested [`execution-plan-v1`](../schemas/execution-plan-v1.schema.json),
and optional [`affected-v2`](../schemas/affected-v2.schema.json) when path sources
narrow the plan.

Stdin: serial interactive runs and `--output raw` (`-j 1`, no `--events`) inherit
caller stdin; parallel or multiplex (`live` / `grouped` / `failures` / `--events`)
runs close stdin for every supervised child.

## 13. Backward compatibility

The following should be considered stable after V1:

- `nxr <app>`;
- `nxr run <app>`;
- root discovery;
- argument forwarding;
- exit-status behavior;
- `list --json` schema versioning;
- plan output versioning;
- completion command names.

New fields may be added to JSON objects. Existing fields should not change meaning within a schema major version.

## 14. Ecosystem ergonomics (2.6)

Design authority: [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md).

```bash
nxr in <shell> <app|task|…>     # ergonomic alias of --shell; never after app name
nxr fmt [PATH…]                 # thin nix fmt / flake formatter (not apps.fmt)
nxr envrc [--shell NAME] [--write] [--force]
nxr doctor env                  # direnv / .envrc / shell integration (informational)
nxr doctor cache                # substituters, trusted keys, discovery/capability cache
nxr doctor builders             # remote builders / nixd (read-only)
nxr build <flake#installable>   # generic installable escape hatch
nxr build --attr <attr-path>
nxr list configurations
nxr inspect configuration <name>
nxr build configuration <name>  # build only; never switch/activate
```

Parsing invariant preserved: runner options stay before the target name
(`nxr --shell backend test` / `nxr in backend test`). Reject forms that place
`--shell` after the app.

`nxr envrc` uses the global `--shell` flag for a named shell (`nxr --shell backend envrc`).
`--write` refuses to overwrite an existing `.envrc` without `--force` (exit 2).

## 15. Automation scaffolding (2.8)

Design authority: [ADR-0148](adr/0148-automation-ergonomics.md).

```bash
nxr init <rust|node|mixed|monorepo> [--template <name>] [--dir PATH] [--yes]
nxr migrate justfile [PATH] [--write PATH]
nxr migrate mise [PATH] [--write PATH]
```

`nxr init` writes embedded flake templates using `nxr.flakeModules.default`. Without
`--yes`, interactive confirmation is required (exit 2 when stdin/stderr are not TTYs).
Existing target paths are never overwritten.

`nxr migrate` reads Justfile or `mise.toml` and prints a suggested `perSystem`
fragment (`nxr.apps` and optional `nxr.tasks`). It never executes migrated recipes.
Output is best-effort; review limitations in the generated comments before committing.

## 16. Planned command surface (not yet shipped)

Design authority: [EXECUTION_CONTEXT.md](EXECUTION_CONTEXT.md) and [ROADMAP.md](ROADMAP.md).
Do not treat these as stable until the named release ships.

### 3.0 execution contexts

```bash
nxr context <name> <app|task …>
nxr context run <name> script <path-or-name> [--] [args...]
```

Contexts, secret references, task I/O, and dependency states require **task
document schema v2**. Plans may show secret refs with `"value": "<runtime>"`
only—never plaintext.

### 3.1 processes

```bash
nxr up [name…]
nxr status
nxr logs <name>
```

### Optional cache daemon (perf Wave 4a)

```bash
nxr daemon start [--foreground] [--socket PATH]
nxr daemon stop [--socket PATH]
nxr daemon status [--socket PATH]
```

Optional; never required for `nxr list` / `plan` / `run`. See
[ADR-0157](adr/0157-optional-nxrd.md) and [PERFORMANCE.md](PERFORMANCE.md).
`NXR_DAEMON=off` refuses connect. Not the V3.3 control plane (ADR-0301/0302).
