# Generated CLI reference

This file is generated from Clap help output. Regenerate with:

```bash
cargo run -p xtask -- cli-ref
```

<!-- BEGIN GENERATED -->

## `nxr --help`

```text
Nix-native flake app runner

Usage: nxr [OPTIONS] [COMMAND]

Commands:
  list        List available flake apps (and tasks), or a specific output kind
  run         Run a flake app
  script      Run a local workspace script (path or `.nxr/scripts/<name>`)
  build       Build a flake package (`nix build`)
  check       Build a flake check, or run `nix flake check` when omitted
  shell       Enter a development shell (`nix develop`)
  plan        Show execution plan
  select      Open interactive selector
  doctor      Diagnose environment and flake configuration
  explain     Explain resolution and invocation for an app or task
  completion  Generate shell completion script
  inspect     Inspect flake metadata
  task        Run a V2 task
  watch       Watch and rerun on filesystem changes
  graph       Show task graph
  cache       Manage nxr discovery cache
  daemon      Optional local cache/coordination daemon (`nxrd`)
  history     Show recent run summaries persisted under XDG state
  affected    Report apps and tasks likely affected by changed paths
  fmt         Format Nix sources via `nix fmt` / the flake formatter
  envrc       Generate direnv `.envrc` content (`use flake` / `use flake .#<shell>`)
  init        Scaffold a minimal nxr flake from a template
  migrate     Suggest nxr Nix from Justfile or mise.toml (never executes recipes)
  context     Named execution contexts (list, inspect, run)
  in          Ergonomic dev-shell prefix: `nxr in <shell> <app|task|…>` (alias of `--shell`)
  ci          CI planning helpers
  trust       Manage project trust for secret-bearing and confirmation-gated tasks
  inventory   List schema-described flake inventory outputs
  up          Start long-running process nodes
  status      Show supervised process status
  logs        Tail logs for a supervised process
  down        Stop supervised process nodes

Options:
  -f, --flake <FLAKE>
          Select flake reference

  -C, --cwd <PATH>
          Set child working directory

      --root
          Run child from flake root

      --dry-run
          Print plan without execution

      --json
          Emit JSON for data-returning commands

      --nix <PATH>
          Override Nix executable

  -s, --select
          Open interactive app selector

      --refresh-discovery
          Ignore nxr discovery cache

      --offline
          Forward `--offline` to Nix when supported

      --accept-flake-config
          Forward `--accept-flake-config` to Nix when supported

      --nix-option <KEY=VAL>
          Forward `--option KEY VAL` to Nix (repeatable; `KEY=VAL`)

      --nix-arg <ARG>
          Forward arbitrary Nix argv fragments (repeatable)

      --shell <NAME>
          Execute through a named `devShell` (`nix develop <flake>#<name> -c <nix> run …`)

      --shell-mode <MODE>
          When to wrap in `--shell` (`smart` skips when `NXR_DEV_SHELL` matches)

          Possible values:
          - smart:  Skip `nix develop` when `NXR_DEV_SHELL` matches `--shell` (default)
          - always: Always wrap when `--shell` is set, even when the marker matches
          - never:  Never wrap; `--shell` is ignored
          
          [default: smart]

      --clean-env
          Run with reduced inherited environment

      --keep-env <NAME>
          Preserve variable in clean mode (repeatable)

      --set-env <KEY=VALUE>
          Set or replace a variable (`KEY=VALUE`, repeatable)

      --unset-env <NAME>
          Remove a variable (repeatable)

      --context <NAME>
          Named execution context for script/task runs (schema v2)

  -q, --quiet...
          Suppress non-error nxr messages

  -v, --verbose...
          Increase runner diagnostics

      --plain
          Disable decorative terminal output

      --no-color
          Disable runner color

      --color <WHEN>
          When to colorize runner output
          
          [default: auto]
          [possible values: auto, always, never]

      --log-format <FORMAT>
          Format for runner diagnostics on stderr
          
          [default: human]
          [possible values: human, plain, json]

      --output <MODE>
          Multiplexed task stdout/stderr mode (parallel runs; default: unlabeled)

          Possible values:
          - live:     Prefix each output line with `[node] ` as chunks arrive
          - grouped:  Buffer stdout/stderr per node; flush when the node exits
          - failures: Buffer per node; emit buffered output only on nonzero [`Event::NodeExited`]
          - summary:  One-line status table per node (no multiplexed child logs)
          - raw:      Single foreground child inherits stdio (no pipe multiplexing)

      --events <FORMAT>
          Emit machine-readable task execution events

          Possible values:
          - jsonl: One JSON-encoded [`Event`] per line

      --log-dir <PATH>
          Tee per-node stdout/stderr into PATH (`<node>.stdout` / `<node>.stderr`)

      --report <KIND=PATH>
          Opt-in post-run report writers (`junit=PATH`, `sarif=PATH`, …)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## `nxr task --help`

```text
Run a V2 task

Usage: nxr task [OPTIONS] [TASKS]... [-- <ARGS>...]

Arguments:
  [TASKS]...
          Task names (union DAG; shared dependencies run once). Optional with `--affected` / `changed`

  [ARGS]...
          Arguments forwarded to each root task's app only (MVP)

Options:
  -f, --flake <FLAKE>
          Select flake reference

  -j, --jobs <N>
          Maximum parallel task nodes
          
          [default: 1]

  -C, --cwd <PATH>
          Set child working directory

      --keep-going
          Continue independent work after a failure (default: fail-fast)

      --root
          Run child from flake root

      --watch
          Watch flake root and rerun on changes

      --debounce <DEBOUNCE>
          Debounce window in milliseconds (`--watch` only)

      --dry-run
          Print plan without execution

      --affected
          Run the union DAG of affected tasks (requires a path source)

      --json
          Emit JSON for data-returning commands

      --base <REF>
          Collect changed paths from `git diff --name-only <base>...HEAD`

      --nix <PATH>
          Override Nix executable

  -s, --select
          Open interactive app selector

      --working-tree
          Include unstaged, staged, and untracked working-tree paths

      --all-changes <REF>
          Union of `--base <ref>` range and `--working-tree`

      --refresh-discovery
          Ignore nxr discovery cache

      --offline
          Forward `--offline` to Nix when supported

      --strict
          Include unknown tasks in the affected set (default unless `--no-strict`)

      --accept-flake-config
          Forward `--accept-flake-config` to Nix when supported

      --no-strict
          Omit unknown tasks from the affected set

      --nix-option <KEY=VAL>
          Forward `--option KEY VAL` to Nix (repeatable; `KEY=VAL`)

      --path <PATH>
          Explicit repository-relative changed paths (with `--affected` or `changed`)

      --junit <PATH>
          Write JUnit XML to PATH after the run

      --nix-arg <ARG>
          Forward arbitrary Nix argv fragments (repeatable)

      --sarif <PATH>
          Write SARIF 2.1.0 to PATH after the run

      --shell <NAME>
          Execute through a named `devShell` (`nix develop <flake>#<name> -c <nix> run …`)

      --coverage <PATH>
          Write coverage JSON stub to PATH after the run

      --shell-mode <MODE>
          When to wrap in `--shell` (`smart` skips when `NXR_DEV_SHELL` matches)

          Possible values:
          - smart:  Skip `nix develop` when `NXR_DEV_SHELL` matches `--shell` (default)
          - always: Always wrap when `--shell` is set, even when the marker matches
          - never:  Never wrap; `--shell` is ignored
          
          [default: smart]

      --benchmark <PATH>
          Write benchmark JSON stub to PATH after the run

      --clean-env
          Run with reduced inherited environment

      --keep-env <NAME>
          Preserve variable in clean mode (repeatable)

      --set <NAME=VALUE>
          Set a typed task parameter (`NAME=VALUE`, repeatable; fail-closed when required)

      --set-env <KEY=VALUE>
          Set or replace a variable (`KEY=VALUE`, repeatable)

      --unset-env <NAME>
          Remove a variable (repeatable)

      --context <NAME>
          Named execution context for script/task runs (schema v2)

  -q, --quiet...
          Suppress non-error nxr messages

  -v, --verbose...
          Increase runner diagnostics

      --plain
          Disable decorative terminal output

      --no-color
          Disable runner color

      --color <WHEN>
          When to colorize runner output
          
          [default: auto]
          [possible values: auto, always, never]

      --log-format <FORMAT>
          Format for runner diagnostics on stderr
          
          [default: human]
          [possible values: human, plain, json]

      --output <MODE>
          Multiplexed task stdout/stderr mode (parallel runs; default: unlabeled)

          Possible values:
          - live:     Prefix each output line with `[node] ` as chunks arrive
          - grouped:  Buffer stdout/stderr per node; flush when the node exits
          - failures: Buffer per node; emit buffered output only on nonzero [`Event::NodeExited`]
          - summary:  One-line status table per node (no multiplexed child logs)
          - raw:      Single foreground child inherits stdio (no pipe multiplexing)

      --events <FORMAT>
          Emit machine-readable task execution events

          Possible values:
          - jsonl: One JSON-encoded [`Event`] per line

      --log-dir <PATH>
          Tee per-node stdout/stderr into PATH (`<node>.stdout` / `<node>.stderr`)

      --report <KIND=PATH>
          Opt-in post-run report writers (`junit=PATH`, `sarif=PATH`, …)

  -h, --help
          Print help (see a summary with '-h')
```

<!-- END GENERATED -->
