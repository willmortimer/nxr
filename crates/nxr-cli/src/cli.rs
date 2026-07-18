//! Clap-derived CLI definition.

use clap::{Parser, Subcommand};

/// Nix-native flake app runner.
#[derive(Debug, Parser)]
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

    /// Emit JSON for data-returning commands
    #[arg(long = "json", global = true)]
    pub json: bool,

    /// Override Nix executable
    #[arg(long = "nix", global = true, value_name = "PATH")]
    pub nix: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level commands. Bare `nxr` defaults to listing apps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// List available flake apps
    List,
    /// Run a flake app
    Run,
    /// Open interactive selector
    Select,
    /// Show execution plan
    Plan,
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
}

impl Command {
    pub const fn label(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Run => "run",
            Self::Select => "select",
            Self::Plan => "plan",
            Self::Doctor => "doctor",
            Self::Completion => "completion",
            Self::Inspect => "inspect",
            Self::Task => "task",
            Self::Watch => "watch",
            Self::Graph => "graph",
        }
    }
}
