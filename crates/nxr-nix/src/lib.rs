//! Nix executable discovery, capability detection, and app resolution.

pub mod adapter;
pub mod capabilities;
pub mod command;
pub mod discovery;
pub mod resolve;
pub mod suggest;
pub mod tasks;

use camino::Utf8PathBuf;
use nxr_core::sanitize::sanitize_terminal_text;

pub use adapter::NixAdapter;
pub use capabilities::{NixFailureKind, detect_system, locate_nix, run_nix};
pub use command::{
    NIX_EXECUTABLE_ENV, current_system_args, flake_eval_json_args, flake_show_args, nix_run_args,
};
pub use discovery::{discover_apps, parse_apps_from_flake_show};
pub use resolve::{AppNotFoundError, resolve_app_by_name};
pub use suggest::{DEFAULT_SUGGESTION_LIMIT, rank_app_suggestions};
pub use tasks::{TaskDiscoveryError, discover_tasks, parse_task_document, tasks_attr_path};

/// Errors from the Nix adapter boundary.
#[derive(Debug)]
pub enum NixError {
    /// `nix` was not found at the expected location.
    NixNotFound { path: Utf8PathBuf },

    /// Failed to spawn `nix`.
    SpawnFailed {
        nix: Utf8PathBuf,
        source: std::io::Error,
    },

    /// `builtins.currentSystem` returned unusable output.
    InvalidSystemOutput,

    /// A `nix` subprocess exited unsuccessfully.
    CommandFailed {
        nix: Utf8PathBuf,
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
        kind: NixFailureKind,
    },

    /// `nix` stdout was not valid JSON.
    InvalidJson { source: serde_json::Error },

    /// Flake show JSON could not be normalized into apps.
    ParseApps(ParseAppsError),
}

impl std::error::Error for NixError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpawnFailed { source, .. } => Some(source),
            Self::InvalidJson { source } => Some(source),
            Self::ParseApps(error) => Some(error),
            Self::NixNotFound { .. } | Self::InvalidSystemOutput | Self::CommandFailed { .. } => {
                None
            }
        }
    }
}

impl From<ParseAppsError> for NixError {
    fn from(error: ParseAppsError) -> Self {
        Self::ParseApps(error)
    }
}

impl std::fmt::Display for NixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl NixError {
    /// User-facing message with command context and sanitized subprocess output.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NixNotFound { path } => {
                format!(
                    "nix executable not found at `{}` (set {} or ensure `nix` is on PATH)",
                    path,
                    command::NIX_EXECUTABLE_ENV
                )
            }
            Self::SpawnFailed { nix, source } => {
                format!("failed to run `{nix}`: {source}")
            }
            Self::InvalidSystemOutput => {
                "nix returned an invalid current system string (try `nix eval --raw --impure --expr builtins.currentSystem`)"
                    .to_owned()
            }
            Self::CommandFailed {
                nix,
                args,
                status,
                stderr,
                kind,
            } => {
                let action = match kind {
                    NixFailureKind::Capability => "detect Nix capabilities",
                    NixFailureKind::Evaluation => "evaluate flake",
                };
                let command = format_nix_invocation(nix, args);
                let status = status
                    .map_or_else(|| "exited with an unknown status".to_owned(), |code| {
                        format!("exited with status {code}")
                    });
                let detail = sanitize_terminal_text(stderr.trim());
                let detail = if detail.is_empty() {
                    "no stderr output".to_owned()
                } else {
                    detail
                };

                format!("failed to {action} (`{command}`; {status}): {detail}")
            }
            Self::InvalidJson { source } => {
                format!("nix output was not valid JSON: {source}")
            }
            Self::ParseApps(error) => error.to_string(),
        }
    }

    /// Stable `nxr` exit code for this adapter error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;

        match self {
            Self::NixNotFound { .. }
            | Self::SpawnFailed { .. }
            | Self::InvalidSystemOutput
            | Self::CommandFailed {
                kind: NixFailureKind::Capability,
                ..
            } => exit::NIX_CAPABILITY,
            Self::CommandFailed {
                kind: NixFailureKind::Evaluation,
                ..
            }
            | Self::InvalidJson { .. }
            | Self::ParseApps { .. } => exit::EVALUATION,
        }
    }
}

fn format_nix_invocation(nix: &Utf8PathBuf, args: &[String]) -> String {
    let mut command = nix.as_str().to_owned();
    for arg in args {
        if arg.contains(char::is_whitespace) {
            command.push(' ');
            command.push('"');
            command.push_str(arg);
            command.push('"');
        } else {
            command.push(' ');
            command.push_str(arg);
        }
    }
    command
}

/// Errors while parsing app metadata from flake show JSON.
#[derive(Debug, thiserror::Error)]
pub enum ParseAppsError {
    /// Reserved for future structured parse failures.
    #[error("failed to parse apps from flake show output")]
    InvalidStructure,
}
