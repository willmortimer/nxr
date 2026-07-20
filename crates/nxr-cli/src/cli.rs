//! Clap-derived CLI definition.

use clap::{ArgAction, Parser, Subcommand};

use nxr_completion::{CompleteTarget, Shell};

use crate::commands::graph::GraphFormat;
use crate::commands::list::ListKind;
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

/// Top-level commands. Bare `nxr` defaults to listing apps.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// List available flake apps (and tasks), or a specific output kind
    List {
        /// Catalog to list (`apps`, `checks`, `packages`, `shells`, `tasks`).
        /// Default: apps + tasks.
        #[arg(value_enum)]
        kind: Option<ListKind>,
        /// Include only tasks in this category (when tasks are listed)
        #[arg(long = "category", value_name = "NAME")]
        category: Option<String>,
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
    /// Build a flake package (`nix build`)
    Build {
        /// Package name (`packages.<system>.<name>`); default package when omitted
        name: Option<String>,
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
        /// App or task name (apps win when both exist)
        app: String,
        /// Arguments included in the plan (pass after `--`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Open interactive selector
    Select,
    /// Diagnose environment and flake configuration
    Doctor {
        /// Run clean-environment diagnostics (may dry-run plan only)
        #[arg(long = "clean-env")]
        clean_env: bool,
        /// Emit extra non-destructive findings (descriptions, naming, cache)
        #[arg(long = "all")]
        all: bool,
        /// Optional app name to validate
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
        /// Include only tasks in this category (overview only)
        #[arg(long = "category", value_name = "NAME")]
        category: Option<String>,
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
        #[arg(long = "watch")]
        watch: bool,
        /// Debounce window in milliseconds (`--watch` only)
        #[arg(long = "debounce", requires = "watch")]
        debounce: Option<u64>,
        /// Task names (union DAG; shared dependencies run once)
        #[arg(required = true)]
        tasks: Vec<String>,
        /// Arguments forwarded to each root task's app only (MVP)
        #[arg(last = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Watch and rerun on filesystem changes
    Watch {
        /// App or task name (task wins when both exist)
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
    /// Report apps and tasks likely affected by changed paths
    Affected {
        /// Collect changed paths from `git diff --name-only <base>...HEAD`
        #[arg(long = "base", value_name = "REF")]
        base: Option<String>,
        /// Explicit repository-relative changed paths
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Bare `nxr <app> [args…]` form (reserved names win first)
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// `nxr cache` subcommands.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum CacheSubcommand {
    /// Remove all discovery cache entries
    Clear,
    /// Show discovery cache location and size
    Status,
}
