//! Nix executable discovery, capability detection, and app resolution.

pub mod adapter;
pub mod capabilities;
pub mod command;
pub mod discovery;
pub mod resolve;
pub mod suggest;

use camino::Utf8PathBuf;

pub use adapter::NixAdapter;
pub use capabilities::{NixFailureKind, detect_system, locate_nix, run_nix};
pub use command::{NIX_EXECUTABLE_ENV, current_system_args, flake_show_args, nix_run_args};
pub use discovery::{discover_apps, parse_apps_from_flake_show};
pub use resolve::{AppNotFoundError, resolve_app_by_name};
pub use suggest::{DEFAULT_SUGGESTION_LIMIT, rank_app_suggestions};

/// Errors from the Nix adapter boundary.
#[derive(Debug, thiserror::Error)]
pub enum NixError {
    /// `nix` was not found at the expected location.
    #[error("nix executable not found at {path}")]
    NixNotFound { path: Utf8PathBuf },

    /// Failed to spawn `nix`.
    #[error("failed to run {nix}: {source}")]
    SpawnFailed {
        nix: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `builtins.currentSystem` returned unusable output.
    #[error("nix returned an invalid current system string")]
    InvalidSystemOutput,

    /// A `nix` subprocess exited unsuccessfully.
    #[error("nix command failed (status {status:?}): {stderr}")]
    CommandFailed {
        nix: Utf8PathBuf,
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
        kind: NixFailureKind,
    },

    /// `nix` stdout was not valid JSON.
    #[error("nix output was not valid JSON: {source}")]
    InvalidJson {
        #[source]
        source: serde_json::Error,
    },

    /// Flake show JSON could not be normalized into apps.
    #[error(transparent)]
    ParseApps(#[from] ParseAppsError),
}

impl NixError {
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

/// Errors while parsing app metadata from flake show JSON.
#[derive(Debug, thiserror::Error)]
pub enum ParseAppsError {
    /// Reserved for future structured parse failures.
    #[error("failed to parse apps from flake show output")]
    InvalidStructure,
}
