# flake-parts module: declarative nxr.tasks (orchestration metadata).
#
# Authors declare tasks under `perSystem.nxr.tasks`. The parent flake-parts
# entry module emits a versioned document at flake output `nxr.<system>`:
#
#   { schema_version = 1; tasks = { ... }; }
#
# matching the Rust `TaskDocument` / `schemas/task-v1.schema.json` contract.
{
  lib,
  ...
}:
let
  inherit (lib) types;

  taskOutputMode = types.enum [
    "replace"
    "merge"
    "verify-only"
    "report"
  ];

  taskCacheMode = types.enum [
    "disabled"
    "local"
    "shared-read"
    "shared"
  ];

  ioIntensity = types.enum [
    "light"
    "normal"
    "heavy"
  ];

  envInputBindingType = types.submodule {
    options = {
      name = lib.mkOption {
        type = types.str;
        description = "Environment variable name.";
      };

      required = lib.mkOption {
        type = types.bool;
        default = false;
        description = "When true, the task fails if the variable is unset.";
      };

      secret = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, the variable is secret metadata: values never appear in
          plans or events, and the action key records presence as `"secret"`
          rather than the value. Auto-disabling workspace cache for
          secret-bearing tasks is tracked as a follow-up (nxr#1); until then
          set `cache.mode = "disabled"` explicitly when outputs depend on the
          secret value.
        '';
      };
    };
  };

  taskInputBindingType = types.submodule {
    options = {
      from = lib.mkOption {
        type = types.str;
        description = "Upstream task output reference (`<task>.<output>`).";
      };
    };
  };

  taskInputsType = types.submodule {
    options = {
      paths = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Repository-relative paths and globs hashed into the task cache key.
        '';
      };

      env = lib.mkOption {
        type = types.listOf (types.either types.str envInputBindingType);
        default = [ ];
        description = ''
          Environment variable names or structured bindings included in the
          cache key. The full inherited environment is never hashed.
        '';
      };

      includeGitState = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, Git tree/commit state participates in the cache key.
        '';
      };

      bindings = lib.mkOption {
        type = types.attrsOf taskInputBindingType;
        default = { };
        description = ''
          Named bindings to upstream task outputs (`<task>.<output>`).
        '';
      };
    };
  };

  taskOutputType = types.submodule {
    options = {
      path = lib.mkOption {
        type = types.str;
        description = "Repository-relative output path under the flake root.";
      };

      mode = lib.mkOption {
        type = types.nullOr taskOutputMode;
        default = null;
        description = ''
          How a cached artifact is restored (`replace`, `merge`, `verify-only`,
          or `report`).
        '';
      };

      optional = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, a missing output at save time does not fail the task.
        '';
      };
    };
  };

  taskCacheType = types.submodule {
    options = {
      mode = lib.mkOption {
        type = types.nullOr taskCacheMode;
        default = null;
        description = ''
          Cache scope (`disabled`, `local`, `shared-read`, or `shared`).
          Only local CAS is implemented today; `shared-read` / `shared` still
          use the local path (honest reject-or-warn is nxr#2). Prefer `local`
          or `disabled` until a shared transport ships.
        '';
      };

      version = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Author-controlled cache salt incremented when task behavior changes
          without input changes.
        '';
      };

      restore = lib.mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = ''
          When true, a cache hit restores declared outputs instead of executing.
        '';
      };

      save = lib.mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = ''
          When true, successful runs with declared outputs may be cached.
        '';
      };

      failures = lib.mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "When true, failed runs may be cached.";
      };
    };
  };

  taskResourcesType = types.submodule {
    options = {
      cpu = lib.mkOption {
        type = types.nullOr types.number;
        default = null;
        description = "Scheduler CPU token demand (not a hard OS quota).";
      };

      memory = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Estimated peak memory reservation (e.g. `4GiB`).";
      };

      io = lib.mkOption {
        type = types.nullOr ioIntensity;
        default = null;
        description = "Relative I/O intensity for scheduling heuristics.";
      };

      network = lib.mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = ''
          When true, the task is expected to use the network (diagnostic and
          CI policy metadata).
        '';
      };

      exclusive = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Named mutexes; at most one in-flight task may hold each lock.
        '';
      };
    };
  };

  taskType = types.submodule {
    options = {
      description = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional short description for listings and completion.";
      };

      dependsOn = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Task names that must complete before this task runs.";
      };

      app = lib.mkOption {
        type = types.str;
        description = "Flake app leaf name (apps.<system>.<name>) this task runs.";
      };

      workingDirectory = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional working-directory policy or flake-root-relative path.
          Accepted values: `invocation`, `flake-root`, or a relative path
          (absolute paths are rejected by the runner). CLI `--root` / `--cwd`
          override this field for every node in a task run.
        '';
      };

      hidden = lib.mkOption {
        type = types.bool;
        default = false;
        description = "When true, the task is omitted from default listings.";
      };

      category = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional logical category for grouping in listings.";
      };

      aliases = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Optional alternate names resolved by explicit task commands (`nxr task`,
          `nxr graph`, `nxr inspect task`, `nxr watch`, and `nxr plan` when the
          name is not an app). Bare `nxr <name>` remains app-only.
        '';
      };

      interactive = lib.mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, the node requires exclusive terminal access: stdin and the
          controlling TTY are inherited, the scheduler runs it alone (no
          concurrent peers), and multiplexed `--output` modes are rejected.
        '';
      };

      paths = lib.mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Optional repository-relative path roots for conservative affected
          analysis (`nxr affected`). Changes under these paths mark the task
          (and its dependents) as affected.
        '';
      };

      timeout = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional wall-clock timeout for this task's process (e.g. `10m`,
          `30s`, `500ms`). When exceeded, nxr terminates the node and records
          a `timed_out` outcome.
        '';
      };

      terminationGracePeriod = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional grace period after timeout or interrupt before SIGKILL
          (e.g. `5s`). Defaults to the runner's built-in grace when unset.
        '';
      };

      shell = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional `devShells.<name>` for this task (shell-only context).
          CLI `--shell` / `nxr in` overrides this field.
        '';
      };

      context = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional named execution context (`perSystem.nxr.contexts.<name>`).
          Overrides `shell` when both are set. CLI context/shell flags override
          task metadata.
        '';
      };

      inputs = lib.mkOption {
        type = types.nullOr taskInputsType;
        default = null;
        description = ''
          Declared inputs for cache-key fingerprinting and structured upstream
          bindings (schema v2).
        '';
      };

      outputs = lib.mkOption {
        type = types.listOf taskOutputType;
        default = [ ];
        description = ''
          Declared workspace outputs that may be restored from a task result
          cache (schema v2).
        '';
      };

      cache = lib.mkOption {
        type = types.nullOr taskCacheType;
        default = null;
        description = "Opt-in task result cache policy (schema v2).";
      };

      resources = lib.mkOption {
        type = types.nullOr taskResourcesType;
        default = null;
        description = ''
          Estimated resource demand and named exclusivity locks for cooperative
          scheduling (schema v2).
        '';
      };
    };
  };
in
{
  options.nxr.schemaVersion = lib.mkOption {
    type = lib.types.nullOr (lib.types.enum [
      1
      2
    ]);
    default = null;
    description = ''
      Force the emitted `nxr.<system>.schema_version`. When unset, nxr
      auto-selects `2` when contexts, processes, or task v2 fields are present,
      otherwise
      `1`. Setting `1` while v2 fields are present fails evaluation; setting
      `2` always emits schema version 2.
    '';
  };

  options.nxr.tasks = lib.mkOption {
    type = types.attrsOf taskType;
    default = { };
    description = ''
      Declarative task definitions. Emitted as the `tasks` map inside the
      versioned flake output `nxr.<system>` (see docs/TASKS.md).
    '';
  };
}
