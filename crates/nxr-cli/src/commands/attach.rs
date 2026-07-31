//! `nxr attach` — reopen the DAG watch for a recorded run.

use std::io::{self, IsTerminal};

use nxr_core::diagnostics::exit;

use crate::runner_output::RunnerOutput;
use crate::tui::{AttachSessionError, resolve_attach_run, run_attach_replay};

/// Errors from `nxr attach`.
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error(transparent)]
    Session(#[from] AttachSessionError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("attach requires an interactive terminal")]
    NotTty,
}

impl AttachError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Session(AttachSessionError::NotFound)
            | Self::Session(AttachSessionError::UnknownRun { .. })
            | Self::NotTty => exit::USAGE,
            Self::Session(AttachSessionError::Io(_)) | Self::Io(_) => exit::EVALUATION,
            Self::Session(AttachSessionError::InvalidSidecar(_)) => exit::EVALUATION,
        }
    }
}

/// Reopen the TUI for a recorded run.
///
/// # Errors
///
/// Returns [`AttachError`] when no attachable run exists or the terminal cannot
/// be opened.
pub fn run(run_id: Option<&str>, runner: RunnerOutput) -> Result<(), AttachError> {
    if !io::stderr().is_terminal() {
        return Err(AttachError::NotTty);
    }

    let session = resolve_attach_run(run_id)?;
    runner
        .info(format!(
            "attach {} ({})",
            session.run_id,
            session.target
        ))
        .map_err(AttachError::Io)?;

    let follow = matches!(session.status, crate::tui::AttachRunStatus::Running);
    run_attach_replay(&session, "nxr attach", follow)?;
    Ok(())
}
