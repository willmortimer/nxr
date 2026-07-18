//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod error_format;
mod flake;
mod output;
mod output_options;
mod runner_output;

use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;

use crate::cli::{Cli, Command};
use crate::commands::common::{AppRequest, DiscoverRequest};
use crate::commands::{
    UnimplementedCommandError, complete, completion, doctor, list, plan, run, select,
};
use crate::error_format::format_error_message;
use crate::output_options::OutputOptions;
use crate::runner_output::RunnerOutput;

fn main() {
    let cli = Cli::parse();
    let output = output_options_from_cli(&cli);
    let runner = RunnerOutput::new(output);
    let result = dispatch(&cli, runner);

    match result {
        Ok(code) => process::exit(code),
        Err(error) => {
            let _ = runner.error(format_error_message(&error));
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
    #[error(transparent)]
    Select(#[from] select::SelectError),
    #[error(transparent)]
    Doctor(#[from] doctor::DoctorError),
    #[error("missing app name")]
    MissingAppName,
    #[error(transparent)]
    Completion(#[from] completion::CompletionError),
    #[error(transparent)]
    Complete(#[from] complete::CompleteError),
    #[error(transparent)]
    Unimplemented(#[from] UnimplementedCommandError),
}

impl RunError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Run(error) => error.exit_code(),
            Self::Plan(error) => error.exit_code(),
            Self::Select(error) => error.exit_code(),
            Self::Doctor(error) => error.exit_code(),
            Self::Completion(_) => completion::CompletionError::exit_code(),
            Self::Complete(_) => exit::SUCCESS,
            Self::MissingAppName => exit::USAGE,
            Self::Unimplemented(_) => UnimplementedCommandError::exit_code(),
        }
    }
}

fn output_options_from_cli(cli: &Cli) -> OutputOptions {
    OutputOptions::new(cli.quiet, cli.verbose, cli.plain, cli.no_color, cli.color)
}

fn dispatch(cli: &Cli, runner: RunnerOutput) -> Result<i32, RunError> {
    match &cli.command {
        None if cli.select => run_with_selected_app(cli, &[], runner),
        None | Some(Command::List) => {
            list::run(
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                cli.json,
                cli.refresh,
                runner,
            )?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Select) => run_with_selected_app(cli, &[], runner),
        Some(Command::Run { app, args }) => {
            if cli.select {
                run_with_selected_app(cli, args, runner)
            } else {
                let request = app_request(cli, app, args);
                run::execute(request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Plan { app, args }) => {
            let request = app_request(cli, app, args);
            plan::run(request, cli.json, runner)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Doctor { clean_env, app }) => {
            let request = doctor::DoctorRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
                app: app.as_deref(),
                clean_env: *clean_env,
                root: cli.root,
                cwd: cli.cwd.as_deref(),
            };
            doctor::run(request, cli.json, runner).map_err(RunError::from)
        }
        Some(Command::External(tokens)) => {
            let (app, forwarded) = split_external(tokens)?;
            if cli.select {
                run_with_selected_app(cli, forwarded, runner)
            } else {
                let request = app_request(cli, app, forwarded);
                run::execute(request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Completion { shell }) => {
            completion::run(*shell)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Complete { target }) => {
            complete::run(
                *target,
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                cli.refresh,
            )?;
            Ok(exit::SUCCESS)
        }
        Some(command @ (Command::Inspect | Command::Task | Command::Watch | Command::Graph)) => {
            Err(UnimplementedCommandError {
                command: command.label(),
            }
            .into())
        }
    }
}

fn run_with_selected_app(
    cli: &Cli,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let app = select::pick_app_name(discover_request(cli))?;
    let request = app_request(cli, &app, args);
    run::execute(request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

fn discover_request(cli: &Cli) -> DiscoverRequest<'_> {
    DiscoverRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
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
