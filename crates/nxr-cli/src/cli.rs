//! Clap-derived CLI definition.

use clap::{ArgAction, Parser, Subcommand};

use crate::output_options::ColorWhen;

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
    #[arg(long = "refresh", global = true)]
    pub refresh: bool,

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

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level commands. Bare `nxr` defaults to listing apps.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// List available flake apps
    List,
    /// Run a flake app
    Run {
        /// App name
        app: String,
        /// Arguments forwarded to the app (one leading `--` is stripped)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show execution plan
    Plan {
        /// App name
        app: String,
        /// Arguments included in the plan (pass after `--`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Open interactive selector
    Select,
    /// Diagnose environment and flake configuration
    Doctor,
    /// Generate shell completion script
    Completion,
    /// Inspect flake metadata
    Inspect,
    /// Run a V2 task
    Task,
    /// Watch and rerun
    Watch,
    /// Show task graph
    Graph,
    /// Bare `nxr <app> [args…]` form (reserved names win first)
    #[command(external_subcommand)]
    External(Vec<String>),
}

impl Command {
    /// Stable command label for unimplemented-command errors.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Run { .. } => "run",
            Self::Plan { .. } => "plan",
            Self::Select => "select",
            Self::Doctor => "doctor",
            Self::Completion => "completion",
            Self::Inspect => "inspect",
            Self::Task => "task",
            Self::Watch => "watch",
            Self::Graph => "graph",
            Self::External(_) => "external",
        }
    }
}
