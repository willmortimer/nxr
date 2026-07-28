//! Clap-derived CLI definition.

use clap::{ArgAction, Parser, Subcommand};

use nxr_completion::{CompleteTarget, Shell};

use crate::commands::graph::GraphFormat;
use crate::output_options::{ColorWhen, LogFormat};
use crate::output_task::{EventsFormat, TaskOutputMode};
use crate::shell_mode::ShellMode;

/// Nix-native flake app runner.
#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
#[command(
    name = "nxr",
    version,
    about = "Nix-native flake app runner",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Select flake reference
    #[arg(short = 'f', long = "flake", global = true)]
    pub flake: Option<String>,

    /// Set child working directory
    #[arg(short = 'C', long = "cwd", global = true, value_name = "PATH")]
    pub cwd: Option<String>,

    /// Run child from flake root
    #[arg(long = "root", global = true)]
    pub root: bool,

    /// Print plan without execution
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Emit JSON for data-returning commands
    #[arg(long = "json", global = true)]
    pub json: bool,

    /// Override Nix executable
    #[arg(long = "nix", global = true, value_name = "PATH")]
    pub nix: Option<String>,

    /// Open interactive app selector
    #[arg(short = 's', long = "select", global = true)]
    pub select: bool,

    /// Ignore nxr discovery cache
    #[arg(long = "refresh-discovery", global = true)]
    pub refresh_discovery: bool,

    /// Forward `--offline` to Nix when supported
    #[arg(long = "offline", global = true)]
    pub offline: bool,

    /// Forward `--accept-flake-config` to Nix when supported
    #[arg(long = "accept-flake-config", global = true)]
    pub accept_flake_config: bool,

    /// Forward `--option KEY VAL` to Nix (repeatable; `KEY=VAL`)
    #[arg(long = "nix-option", global = true, value_name = "KEY=VAL")]
    pub nix_option: Vec<String>,

    /// Forward arbitrary Nix argv fragments (repeatable)
    #[arg(long = "nix-arg", global = true, value_name = "ARG")]
    pub nix_arg: Vec<String>,

    /// Execute through a named `devShell` (`nix develop <flake>#<name> -c <nix> run …`)
    #[arg(long = "shell", global = true, value_name = "NAME")]
    pub dev_shell: Option<String>,

    /// When to wrap in `--shell` (`smart` skips when `NXR_DEV_SHELL` matches)
    #[arg(
        long = "shell-mode",
        global = true,
        value_enum,
        default_value_t = ShellMode::Smart,
        value_name = "MODE"
    )]
    pub shell_mode: ShellMode,

    /// Run with reduced inherited environment
    #[arg(long = "clean-env", global = true)]
    pub clean_env: bool,

    /// Preserve variable in clean mode (repeatable)
    #[arg(long = "keep-env", global = true, value_name = "NAME")]
    pub keep_env: Vec<String>,

    /// Set or replace a variable (`KEY=VALUE`, repeatable)
    #[arg(long = "set-env", global = true, value_name = "KEY=VALUE")]
    pub set_env: Vec<String>,

    /// Remove a variable (repeatable)
    #[arg(long = "unset-env", global = true, value_name = "NAME")]
    pub unset_env: Vec<String>,

    /// Suppress non-error nxr messages
    #[arg(short = 'q', long = "quiet", global = true, action = ArgAction::Count)]
    pub quiet: u8,

    /// Increase runner diagnostics
    #[arg(short = 'v', long = "verbose", global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Disable decorative terminal output
    #[arg(long = "plain", global = true)]
    pub plain: bool,

    /// Disable runner color
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    /// When to colorize runner output
    #[arg(
        long = "color",
        global = true,
        value_name = "WHEN",
        default_value = "auto"
    )]
    pub color: ColorWhen,

    /// Format for runner diagnostics on stderr
    #[arg(
        long = "log-format",
        global = true,
        value_name = "FORMAT",
        default_value = "human"
    )]
    pub log_format: LogFormat,

    /// Multiplexed task stdout/stderr mode (parallel runs; default: unlabeled)
    #[arg(long = "output", global = true, value_enum, value_name = "MODE")]
    pub output: Option<TaskOutputMode>,

    /// Emit machine-readable task execution events
    #[arg(long = "events", global = true, value_enum, value_name = "FORMAT")]
    pub events: Option<EventsFormat>,

    /// Opt-in post-run report writers (`junit=PATH`, `sarif=PATH`, …)
    #[arg(long = "report", global = true, value_name = "KIND=PATH")]
    pub report: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// `nxr inspect` sub-targets.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum InspectSubcommand {
    /// Inspect a single app
    App {
        /// App name
        name: String,
    },
    /// Inspect a single task
    Task {
        /// Task name
        name: String,
    },
    /// Inspect a flake configuration (`nixosConfigurations`, `darwinConfigurations`, `homeConfigurations`)
    Configuration {
        /// Configuration name
        name: String,
    },
    /// Inspect a custom inventory role entry
    Inventory {
        /// Inventory role (flake output table name)
        role: String,
        /// Leaf entry name within the role
        name: String,
    },
}

/// `nxr explain` sub-targets.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ExplainSubcommand {
    /// Explain a single app
    App {
        /// App name
        name: String,
        /// Arguments included in the explanation (pass after `--`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Explain a single task
    Task {
        /// Task name
        name: String,
        /// Arguments forwarded to the root task app only
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// `nxr doctor` sub-targets.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum DoctorSubcommand {
    /// Determinate Nix distribution and integration diagnostics
    Determinate {
        /// Include extended builder hints from nixd status
        #[arg(long = "all")]
        all: bool,
        /// Bypass the capability cache when probing Nix
        #[arg(long = "refresh")]
        refresh: bool,
    },
    /// Direnv, `.envrc`, and nxr shell-integration diagnostics (informational)
    Env,
    /// Nix substituters, trusted keys, and nxr discovery/capability cache diagnostics
    Cache,
    /// Remote builders and Determinate nixd diagnostics (read-only)
    Builders,
}

/// `nxr build` sub-targets.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum BuildSubcommand {
    /// Build a flake configuration (no switch/activate)
    Configuration {
        /// Configuration name
        name: String,
    },
}

/// Top-level commands. Bare `nxr` defaults to listing apps.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// List available flake apps (and tasks), or a specific output kind
    List {
        /// Catalog (`apps`, `tasks`, …) or selector (`category:<name>`, `changed`)
        #[arg(value_name = "KIND_OR_SELECTOR")]
        filter: Option<String>,
        /// Include only apps/tasks in this category
        #[arg(long = "category", value_name = "NAME")]
        category: Option<String>,
        /// Include only apps/tasks in this project namespace (`nxr.projects.json`)
        #[arg(long = "namespace", value_name = "NAME")]
        namespace: Option<String>,
        /// Collect changed paths from `git diff --name-only <base>...HEAD` (`changed` selector)
        #[arg(long = "base", value_name = "REF", conflicts_with = "all_changes")]
        base: Option<String>,
        /// Include unstaged, staged, and untracked working-tree paths (`changed` selector)
        #[arg(long = "working-tree", conflicts_with = "all_changes")]
        working_tree: bool,
        /// Union of `--base <ref>` range and `--working-tree` (`changed` selector)
        #[arg(
            long = "all-changes",
            value_name = "REF",
            conflicts_with_all = ["base", "working_tree"]
        )]
        all_changes: Option<String>,
        /// Explicit repository-relative changed paths (`changed` selector)
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Run a flake app
    Run {
        /// App name
        app: String,
        /// Watch flake root and rerun on changes
        #[arg(long = "watch")]
        watch: bool,
        /// Debounce window in milliseconds (`--watch` only)
        #[arg(long = "debounce", requires = "watch")]
        debounce: Option<u64>,
        /// Arguments forwarded to the app (one leading `--` is stripped)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a local workspace script (path or `.nxr/scripts/<name>`)
    Script {
        /// Script path (`./scripts/foo.sh`) or convention name (`deploy`)
        #[arg(value_name = "PATH_OR_NAME")]
        path_or_name: String,
        /// Arguments forwarded to the script (one leading `--` is stripped)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build a flake package (`nix build`)
    Build {
        #[command(subcommand)]
        target: Option<BuildSubcommand>,
        /// Package leaf name or explicit installable (`.#attr` or `flake#attr`)
        #[arg(value_name = "INSTALLABLE")]
        installable: Option<String>,
        /// Build a flake attribute path (for example `nixosConfigurations.dev.config.system.build.vm`)
        #[arg(long = "attr", value_name = "ATTR", conflicts_with = "installable")]
        attr: Option<String>,
    },
    /// Build a flake check, or run `nix flake check` when omitted
    Check {
        /// Check name (`checks.<system>.<name>`); all checks when omitted
        name: Option<String>,
    },
    /// Enter a development shell (`nix develop`)
    Shell {
        /// Shell name (`devShells.<system>.<name>`); default shell when omitted
        name: Option<String>,
    },
    /// Show execution plan
    Plan {
        /// App or task name (apps win when both exist); optional with `--affected`
        #[arg(required_unless_present = "affected")]
        app: Option<String>,
        /// Plan the union DAG of affected tasks (requires a path source)
        #[arg(long = "affected")]
        affected: bool,
        /// Collect changed paths from `git diff --name-only <base>...HEAD`
        #[arg(long = "base", value_name = "REF", conflicts_with = "all_changes")]
        base: Option<String>,
        /// Include unstaged, staged, and untracked working-tree paths
        #[arg(long = "working-tree", conflicts_with = "all_changes")]
        working_tree: bool,
        /// Union of `--base <ref>` range and `--working-tree`
        #[arg(
            long = "all-changes",
            value_name = "REF",
            conflicts_with_all = ["base", "working_tree"]
        )]
        all_changes: Option<String>,
        /// Include unknown tasks in the affected set (default unless `--no-strict`)
        #[arg(long = "strict", action = ArgAction::SetTrue, requires = "affected")]
        strict: bool,
        /// Omit unknown tasks from the affected set
        #[arg(
            long = "no-strict",
            action = ArgAction::SetTrue,
            requires = "affected",
            conflicts_with = "strict"
        )]
        no_strict: bool,
        /// Explicit repository-relative changed paths (with `--affected` or `changed`)
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Open interactive selector
    Select,
    /// Diagnose environment and flake configuration
    Doctor {
        #[command(subcommand)]
        target: Option<DoctorSubcommand>,
        /// Run clean-environment diagnostics (may dry-run plan only)
        #[arg(long = "clean-env")]
        clean_env: bool,
        /// Emit extra non-destructive findings (descriptions, naming, cache)
        #[arg(long = "all")]
        all: bool,
        /// Optional app name to validate (default doctor form only)
        app: Option<String>,
    },
    /// Explain resolution and invocation for an app or task
    Explain {
        /// App or task name (apps win when both exist)
        #[arg(value_name = "NAME")]
        name: Option<String>,
        #[command(subcommand)]
        target: Option<ExplainSubcommand>,
        /// Arguments included in the explanation (default form only)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Generate shell completion script
    Completion {
        /// Target shell
        shell: Shell,
    },
    /// Hidden dynamic completion protocol for shell integrations
    #[command(name = "__complete", hide = true)]
    Complete {
        /// Completion target
        target: CompleteTarget,
    },
    /// Hidden man-page generator for packaging
    #[command(name = "__manpage", hide = true)]
    Manpage,
    /// Inspect flake metadata
    Inspect {
        /// Include only apps/tasks in this category (overview only)
        #[arg(long = "category", value_name = "NAME")]
        category: Option<String>,
        /// Include only apps/tasks in this project namespace (`nxr.projects.json`)
        #[arg(long = "namespace", value_name = "NAME")]
        namespace: Option<String>,
        #[command(subcommand)]
        target: Option<InspectSubcommand>,
    },
    /// Run a V2 task
    Task {
        /// Maximum parallel task nodes
        #[arg(short = 'j', long = "jobs", default_value_t = 1, value_name = "N")]
        jobs: usize,
        /// Continue independent work after a failure (default: fail-fast)
        #[arg(long = "keep-going")]
        keep_going: bool,
        /// Watch flake root and rerun on changes
        #[arg(long = "watch", conflicts_with = "affected")]
        watch: bool,
        /// Debounce window in milliseconds (`--watch` only)
        #[arg(long = "debounce", requires = "watch")]
        debounce: Option<u64>,
        /// Run the union DAG of affected tasks (requires a path source)
        #[arg(long = "affected")]
        affected: bool,
        /// Collect changed paths from `git diff --name-only <base>...HEAD`
        #[arg(long = "base", value_name = "REF", conflicts_with = "all_changes")]
        base: Option<String>,
        /// Include unstaged, staged, and untracked working-tree paths
        #[arg(long = "working-tree", conflicts_with = "all_changes")]
        working_tree: bool,
        /// Union of `--base <ref>` range and `--working-tree`
        #[arg(
            long = "all-changes",
            value_name = "REF",
            conflicts_with_all = ["base", "working_tree"]
        )]
        all_changes: Option<String>,
        /// Include unknown tasks in the affected set (default unless `--no-strict`)
        #[arg(long = "strict", action = ArgAction::SetTrue, requires = "affected")]
        strict: bool,
        /// Omit unknown tasks from the affected set
        #[arg(
            long = "no-strict",
            action = ArgAction::SetTrue,
            requires = "affected",
            conflicts_with = "strict"
        )]
        no_strict: bool,
        /// Explicit repository-relative changed paths (with `--affected` or `changed`)
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
        /// Task names (union DAG; shared dependencies run once). Optional with `--affected` / `changed`.
        #[arg(required_unless_present = "affected")]
        tasks: Vec<String>,
        /// Write JUnit XML to PATH after the run
        #[arg(long = "junit", value_name = "PATH")]
        junit: Option<String>,
        /// Write SARIF 2.1.0 to PATH after the run
        #[arg(long = "sarif", value_name = "PATH")]
        sarif: Option<String>,
        /// Write coverage JSON stub to PATH after the run
        #[arg(long = "coverage", value_name = "PATH")]
        coverage: Option<String>,
        /// Write benchmark JSON stub to PATH after the run
        #[arg(long = "benchmark", value_name = "PATH")]
        benchmark: Option<String>,
        /// Arguments forwarded to each root task's app only (MVP)
        #[arg(last = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Watch and rerun on filesystem changes
    Watch {
        /// App or task name (`app:` / `task:` disambiguate; otherwise task wins)
        name: String,
        /// Debounce window in milliseconds
        #[arg(long = "debounce", default_value_t = crate::commands::watch::DEFAULT_DEBOUNCE_MS)]
        debounce: u64,
        /// Only restart when a changed path matches this glob (repeatable)
        #[arg(long = "include", value_name = "GLOB")]
        include: Vec<String>,
        /// Ignore changes under this glob (repeatable; built-in ignores still apply)
        #[arg(long = "exclude", value_name = "GLOB")]
        exclude: Vec<String>,
        /// Clear the terminal before each new generation
        #[arg(long = "clear")]
        clear: bool,
        /// Arguments forwarded to the app (or root task app)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show task graph
    Graph {
        /// Task name
        task: String,
        /// Output format
        #[arg(long = "format", value_enum, default_value = "text")]
        format: GraphFormat,
    },
    /// Manage nxr discovery cache
    Cache {
        #[command(subcommand)]
        action: CacheSubcommand,
    },
    /// Optional local cache/coordination daemon (`nxrd`)
    Daemon {
        #[command(subcommand)]
        action: DaemonSubcommand,
    },
    /// Show recent run summaries persisted under XDG state
    History {
        #[command(subcommand)]
        action: Option<HistorySubcommand>,
    },
    /// Report apps and tasks likely affected by changed paths
    Affected {
        /// Collect changed paths from `git diff --name-only <base>...HEAD`
        #[arg(long = "base", value_name = "REF", conflicts_with = "all_changes")]
        base: Option<String>,
        /// Collect unstaged, staged, and untracked working-tree paths
        #[arg(long = "working-tree", conflicts_with = "all_changes")]
        working_tree: bool,
        /// Union of `--base <ref>` range and `--working-tree`
        #[arg(
            long = "all-changes",
            value_name = "REF",
            conflicts_with_all = ["base", "working_tree"]
        )]
        all_changes: Option<String>,
        /// Include unknown nodes in apps/tasks lists (CI default; on unless `--no-strict`)
        #[arg(long = "strict", action = ArgAction::SetTrue)]
        strict: bool,
        /// Omit unknown nodes from apps/tasks lists (affected only)
        #[arg(long = "no-strict", action = ArgAction::SetTrue, conflicts_with = "strict")]
        no_strict: bool,
        /// Explicit repository-relative changed paths
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Format Nix sources via `nix fmt` / the flake formatter
    Fmt {
        /// Paths to format (default: selected flake)
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Generate direnv `.envrc` content (`use flake` / `use flake .#<shell>`)
    Envrc {
        /// Write `<flake-root>/.envrc` (refuses overwrite without `--force`)
        #[arg(long = "write")]
        write: bool,
        /// Overwrite an existing `.envrc` when using `--write`
        #[arg(long = "force", requires = "write")]
        force: bool,
    },
    /// Scaffold a minimal nxr flake from a template
    Init {
        /// Template to scaffold (`rust`, `node`, `mixed`, `monorepo`)
        #[arg(value_name = "TEMPLATE")]
        template: Option<String>,
        /// Template to scaffold (alternative to positional `TEMPLATE`)
        #[arg(long = "template", value_name = "TEMPLATE")]
        template_long: Option<String>,
        /// Target directory (default: invocation CWD)
        #[arg(long = "dir", value_name = "PATH")]
        dir: Option<String>,
        /// Write files without interactive confirmation
        #[arg(long = "yes")]
        yes: bool,
    },
    /// Suggest nxr Nix from Justfile or mise.toml (never executes recipes)
    Migrate {
        #[command(subcommand)]
        source: MigrateSubcommand,
    },
    /// Named execution contexts (list, inspect, run)
    Context {
        #[command(subcommand)]
        action: ContextSubcommand,
    },
    /// Ergonomic dev-shell prefix: `nxr in <shell> <app|task|…>` (alias of `--shell`)
    In {
        /// Development shell name
        shell: String,
        /// Subcommand (`run`, `plan`, `task`, `watch`, `explain`) or app name
        verb: String,
        /// Arguments forwarded to the target
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// CI planning helpers
    Ci {
        #[command(subcommand)]
        action: CiSubcommand,
    },
    /// Manage project trust for secret-bearing and confirmation-gated tasks
    Trust {
        #[command(subcommand)]
        action: TrustSubcommand,
    },
    /// List schema-described flake inventory outputs
    Inventory {
        /// Filter to one inventory role (flake output table)
        #[arg(long = "role", value_name = "ROLE")]
        role: Option<String>,
    },
    /// Start long-running process nodes
    Up {
        /// Process names (all when omitted)
        #[arg(value_name = "NAME")]
        names: Vec<String>,
    },
    /// Show supervised process status
    Status,
    /// Tail logs for a supervised process
    Logs {
        /// Process name
        name: String,
        /// Follow log output
        #[arg(long = "follow")]
        follow: bool,
    },
    /// Stop supervised process nodes
    Down {
        /// Process names (all running when omitted)
        #[arg(value_name = "NAME")]
        names: Vec<String>,
    },
    /// Bare `nxr <app> [args…]` form (reserved names win first)
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// `nxr context` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ContextSubcommand {
    /// List defined execution contexts
    List,
    /// Inspect a single context (names and refs only)
    Inspect {
        /// Context name
        name: String,
    },
    /// Run a task under a named context
    Run {
        /// Context name
        context: String,
        /// `task <name>`, or shorthand `<task-name>`, plus forwarded args
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

/// `nxr ci` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum CiSubcommand {
    /// Export a provider-neutral CI execution plan
    Plan {
        /// Optional task roots (default: `ci` task or sink tasks)
        #[arg(value_name = "TASK")]
        roots: Vec<String>,
        /// Collect changed paths from `git diff --name-only <base>...HEAD`
        #[arg(long = "base", value_name = "REF", conflicts_with = "all_changes")]
        base: Option<String>,
        /// Include unstaged, staged, and untracked working-tree paths
        #[arg(long = "working-tree", conflicts_with = "all_changes")]
        working_tree: bool,
        /// Union of `--base <ref>` range and `--working-tree`
        #[arg(
            long = "all-changes",
            value_name = "REF",
            conflicts_with_all = ["base", "working_tree"]
        )]
        all_changes: Option<String>,
        /// Include unknown tasks in the affected set (default unless `--no-strict`)
        #[arg(long = "strict", action = ArgAction::SetTrue)]
        strict: bool,
        /// Omit unknown tasks from the affected set
        #[arg(long = "no-strict", action = ArgAction::SetTrue, conflicts_with = "strict")]
        no_strict: bool,
        /// Explicit repository-relative changed paths
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
    },
}

/// `nxr migrate` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MigrateSubcommand {
    /// Suggest nxr Nix from a Justfile
    Justfile {
        /// Justfile path (default: `Justfile` or `justfile` in the invocation CWD)
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// Write output to a file instead of stdout
        #[arg(long = "write", value_name = "PATH")]
        write: Option<String>,
    },
    /// Suggest nxr Nix from mise.toml
    Mise {
        /// mise.toml path (default: `mise.toml` in the invocation CWD)
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// Write output to a file instead of stdout
        #[arg(long = "write", value_name = "PATH")]
        write: Option<String>,
    },
}

/// `nxr cache` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum CacheSubcommand {
    /// Remove discovery, capability, and workspace CAS entries
    Clear,
    /// Show cache locations and sizes
    Status,
    /// Explain discovery cache and/or workspace CAS for a task
    Explain {
        /// Task name for workspace CAS explain (omit for discovery-only explain)
        #[arg(value_name = "TASK")]
        task: Option<String>,
        /// Treat discovery cache as task-inclusive (require cached tasks document)
        #[arg(long = "tasks")]
        tasks: bool,
    },
}

/// `nxr daemon` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum DaemonSubcommand {
    /// Start the local cache daemon (background unless `--foreground`)
    Start {
        /// Run in the foreground (used by background spawn and tests)
        #[arg(long = "foreground")]
        foreground: bool,
        /// Override Unix socket path (`NXR_DAEMON_SOCKET`)
        #[arg(long = "socket", value_name = "PATH")]
        socket: Option<String>,
    },
    /// Stop the local cache daemon
    Stop {
        /// Override Unix socket path
        #[arg(long = "socket", value_name = "PATH")]
        socket: Option<String>,
    },
    /// Show whether the local cache daemon is running
    Status {
        /// Override Unix socket path
        #[arg(long = "socket", value_name = "PATH")]
        socket: Option<String>,
    },
}

/// `nxr history` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum HistorySubcommand {
    /// List recent run summaries (default)
    List,
    /// Remove all persisted run summaries
    Clear,
}

/// `nxr trust` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TrustSubcommand {
    /// Show whether the selected project is trusted
    Status,
    /// Persist trust for the selected project
    Add,
    /// Remove persisted trust for the selected project
    Revoke,
}
