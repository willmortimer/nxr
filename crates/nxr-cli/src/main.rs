//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod flake;
mod output;

use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;

use crate::cli::{Cli, Command};
use crate::commands::common::AppRequest;
use crate::commands::{UnimplementedCommandError, list, plan, run};

fn main() {
    let cli = Cli::parse();
    let result = dispatch(&cli);

    match result {
        Ok(code) => process::exit(code),
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
    Run(#[from] run::RunError),
    #[error(transparent)]
    Plan(#[from] plan::PlanError),
    #[error("missing app name")]
    MissingAppName,
    #[error(transparent)]
    Unimplemented(#[from] UnimplementedCommandError),
}

impl RunError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Run(error) => error.exit_code(),
            Self::Plan(error) => error.exit_code(),
            Self::MissingAppName => exit::USAGE,
            Self::Unimplemented(_) => UnimplementedCommandError::exit_code(),
        }
    }
}

fn dispatch(cli: &Cli) -> Result<i32, RunError> {
    match &cli.command {
        None | Some(Command::List) => {
            list::run(cli.flake.as_deref(), cli.nix.as_deref(), cli.json)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Run { app, args }) => {
            let request = app_request(cli, app, args);
            run::execute(request, cli.dry_run, cli.json).map_err(RunError::from)
        }
        Some(Command::Plan { app, args }) => {
            let request = app_request(cli, app, args);
            plan::run(request, cli.json)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::External(tokens)) => {
            let (app, forwarded) = split_external(tokens)?;
            let request = app_request(cli, app, forwarded);
            run::execute(request, cli.dry_run, cli.json).map_err(RunError::from)
        }
        Some(
            command @ (Command::Select
            | Command::Doctor
            | Command::Completion
            | Command::Inspect
            | Command::Task
            | Command::Watch
            | Command::Graph),
        ) => Err(UnimplementedCommandError {
            command: command.label(),
        }
        .into()),
    }
}

fn app_request<'a>(cli: &'a Cli, app: &'a str, args: &'a [String]) -> AppRequest<'a> {
    AppRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        app,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
    }
}

fn split_external(tokens: &[String]) -> Result<(&str, &[String]), RunError> {
    tokens
        .split_first()
        .map(|(app, forwarded)| (app.as_str(), forwarded))
        .ok_or(RunError::MissingAppName)
}
