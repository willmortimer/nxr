//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod error_format;
mod flake;
mod output;
mod output_options;
mod runner_output;

use std::collections::BTreeMap;
use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, parse_env_name, parse_set_env};

use crate::cli::{Cli, Command};
use crate::commands::common::{AppRequest, DiscoverRequest};
use crate::commands::{
    UnimplementedCommandError, complete, completion, doctor, graph, list, manpage, plan, run,
    select,
};
use crate::error_format::format_error_message;
use crate::flake::{ParseFlakeAppRefError, parse_flake_app_ref};
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
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    FlakeAppRef(#[from] ParseFlakeAppRefError),
    #[error(transparent)]
    Completion(#[from] completion::CompletionError),
    #[error(transparent)]
    Complete(#[from] complete::CompleteError),
    #[error(transparent)]
    Manpage(#[from] manpage::ManpageError),
    #[error(transparent)]
    Graph(#[from] graph::GraphError),
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
            Self::Manpage(_) => manpage::ManpageError::exit_code(),
            Self::Graph(error) => error.exit_code(),
            Self::MissingAppName | Self::Usage(_) | Self::FlakeAppRef(_) => exit::USAGE,
            Self::Unimplemented(_) => UnimplementedCommandError::exit_code(),
        }
    }
}

fn output_options_from_cli(cli: &Cli) -> OutputOptions {
    OutputOptions::new(
        cli.quiet,
        cli.verbose,
        cli.plain,
        cli.no_color,
        cli.color,
        cli.log_format,
    )
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
                let request = app_request(cli, app, args)?;
                run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Plan { app, args }) => {
            let request = app_request(cli, app, args)?;
            plan::run(&request, cli.json, runner)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Doctor {
            clean_env,
            all,
            app,
        }) => {
            let (flake_arg, app) = resolve_doctor_app(cli, app.as_deref())?;
            let request = doctor::DoctorRequest {
                flake_arg,
                nix_override: cli.nix.as_deref(),
                app,
                clean_env: *clean_env || cli.clean_env,
                all: *all,
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
                let request = app_request(cli, app, forwarded)?;
                run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
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
        Some(Command::Manpage) => {
            manpage::run()?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Graph { task, format }) => {
            let request = graph::GraphRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
                task: task.as_str(),
            };
            graph::run(&request, *format, cli.json, runner)?;
            Ok(exit::SUCCESS)
        }
        Some(command @ (Command::Inspect | Command::Task | Command::Watch)) => {
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
    let request = app_request(cli, &app, args)?;
    run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

fn discover_request(cli: &Cli) -> DiscoverRequest<'_> {
    DiscoverRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
    }
}

fn app_request<'a>(
    cli: &'a Cli,
    app: &'a str,
    args: &'a [String],
) -> Result<AppRequest<'a>, RunError> {
    let target = resolve_app_target(cli, app)?;
    Ok(AppRequest {
        flake_arg: target.flake_arg,
        nix_override: cli.nix.as_deref(),
        app: target.app,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        environment_policy: environment_policy_from_cli(cli)?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedAppTarget<'a> {
    flake_arg: Option<&'a str>,
    app: &'a str,
}

fn resolve_app_target<'a>(
    cli: &'a Cli,
    app_token: &'a str,
) -> Result<ResolvedAppTarget<'a>, RunError> {
    if let Some(parsed) = parse_flake_app_ref(app_token)? {
        if cli.flake.is_some() {
            return Err(RunError::Usage(
                "cannot use --flake with an inline flake#app reference".to_owned(),
            ));
        }
        return Ok(ResolvedAppTarget {
            flake_arg: Some(parsed.flake_ref),
            app: parsed.app,
        });
    }

    Ok(ResolvedAppTarget {
        flake_arg: cli.flake.as_deref(),
        app: app_token,
    })
}

fn resolve_doctor_app<'a>(
    cli: &'a Cli,
    app: Option<&'a str>,
) -> Result<(Option<&'a str>, Option<&'a str>), RunError> {
    let Some(app_token) = app else {
        return Ok((cli.flake.as_deref(), None));
    };

    let target = resolve_app_target(cli, app_token)?;
    Ok((target.flake_arg, Some(target.app)))
}

fn environment_policy_from_cli(cli: &Cli) -> Result<EnvironmentPolicy, RunError> {
    let has_overrides =
        !cli.keep_env.is_empty() || !cli.set_env.is_empty() || !cli.unset_env.is_empty();
    if has_overrides && !cli.clean_env {
        return Err(RunError::Usage(
            "--keep-env, --set-env, and --unset-env require --clean-env".to_owned(),
        ));
    }
    if !cli.clean_env {
        return Ok(EnvironmentPolicy::Inherit);
    }

    let mut keep = Vec::with_capacity(cli.keep_env.len());
    for name in &cli.keep_env {
        keep.push(parse_env_name(name).map_err(RunError::Usage)?);
    }

    let mut set = BTreeMap::new();
    for raw in &cli.set_env {
        let (key, value) = parse_set_env(raw).map_err(RunError::Usage)?;
        set.insert(key, value);
    }

    let mut unset = Vec::with_capacity(cli.unset_env.len());
    for name in &cli.unset_env {
        unset.push(parse_env_name(name).map_err(RunError::Usage)?);
    }

    Ok(EnvironmentPolicy::Clean { keep, set, unset })
}

fn split_external(tokens: &[String]) -> Result<(&str, &[String]), RunError> {
    tokens
        .split_first()
        .map(|(app, forwarded)| (app.as_str(), forwarded))
        .ok_or(RunError::MissingAppName)
}
