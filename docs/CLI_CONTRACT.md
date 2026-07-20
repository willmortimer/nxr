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
nxr list
nxr select
nxr plan <app-or-task>
nxr doctor [app]
nxr completion <shell>
nxr cache clear|status
nxr inspect ...
nxr task <task> [args...]
nxr watch <app-or-task>
nxr graph <task>
```

V1 implements the app-oriented subset. V2 activates task-oriented commands.

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
    --offline              Forward `--offline` to Nix when supported
    --accept-flake-config  Forward `--accept-flake-config` to Nix when supported
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
    --watch                Watch and rerun/restart
    --debounce <DURATION>  Watch debounce
```
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
```

`--output raw` requires `-j 1` and cannot be combined with `--events`. It
bypasses line-oriented event conversion so binary and interactive child I/O
pass through unchanged. Multiplexed modes (`live` / `grouped` / `failures`)
close caller stdin for supervised children.

`--output summary` is reserved / not implemented in V2.0.

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
nxr graph ci
nxr watch dev
```

Trailing arguments after the task name are forwarded to the **root task app only**
(`argument_forwarding: root`). Dependency nodes always receive an empty argument
list. Richer per-node forwarding is deferred.

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
