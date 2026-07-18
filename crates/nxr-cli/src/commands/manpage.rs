//! Hidden man-page generation for packaging (`installManPage`).

use std::io::{self, Write};

use clap::CommandFactory;
use clap_mangen::Man;

use crate::cli::Cli;

/// Errors while rendering the man page.
#[derive(Debug, thiserror::Error)]
pub enum ManpageError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ManpageError {
    #[must_use]
    pub const fn exit_code() -> i32 {
        nxr_core::diagnostics::exit::EVALUATION
    }
}

/// Write the `nxr(1)` man page to stdout.
///
/// # Errors
///
/// Returns [`ManpageError`] when writing fails.
pub fn run() -> Result<(), ManpageError> {
    let cmd = Cli::command();
    let man = Man::new(cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&buffer)?;
    stdout.flush()?;
    Ok(())
}
