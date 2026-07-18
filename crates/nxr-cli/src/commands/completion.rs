//! `nxr completion` command implementation.

use std::io::{self, Write};

use clap::CommandFactory;
use nxr_completion::{Shell, generate_script};

use crate::cli::Cli;

/// Errors while generating a completion script.
#[derive(Debug, thiserror::Error)]
pub enum CompletionError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl CompletionError {
    #[must_use]
    pub const fn exit_code() -> i32 {
        nxr_core::diagnostics::exit::EVALUATION
    }
}

/// Print a shell completion script to stdout.
///
/// # Errors
///
/// Returns [`CompletionError`] when writing fails.
pub fn run(shell: Shell) -> Result<(), CompletionError> {
    let mut cmd = Cli::command();
    let mut stdout = io::stdout().lock();
    generate_script(shell, &mut cmd, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}
