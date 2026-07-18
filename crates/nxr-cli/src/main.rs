//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod flake;
mod output;

use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;

use crate::cli::{Cli, Command};
use crate::commands::{UnimplementedCommandError, list};

fn main() {
    let cli = Cli::parse();
    let result = run(&cli);

    match result {
        Ok(()) => process::exit(exit::SUCCESS),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(error.exit_code());
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(transparent)]
    List(#[from] list::ListError),
    #[error(transparent)]
    Unimplemented(#[from] UnimplementedCommandError),
}

impl RunError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Unimplemented(_) => UnimplementedCommandError::exit_code(),
        }
    }
}

fn run(cli: &Cli) -> Result<(), RunError> {
    match cli.command {
        None | Some(Command::List) => {
            list::run(cli.flake.as_deref(), cli.nix.as_deref(), cli.json).map_err(RunError::from)
        }
        Some(command) => Err(UnimplementedCommandError {
            command: command.label(),
        }
        .into()),
    }
}
