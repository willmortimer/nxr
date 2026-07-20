//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod error_format;
mod flake;
mod nix_flags;
mod output;
mod output_options;
mod output_task;
mod runner_output;

use std::collections::BTreeMap;
use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, parse_env_name, parse_set_env};

use crate::cli::{CacheSubcommand, Cli, Command, InspectSubcommand};
use crate::commands::common::{AppRequest, DiscoverRequest};
use crate::commands::{
    cache, complete, completion, doctor, graph, inspect, list, manpage, plan, run, select, task,
    watch,
};
use crate::error_format::format_error_message;
use crate::flake::{ParseFlakeAppRefError, parse_flake_app_ref};
use crate::nix_flags::nix_flags_from_cli;
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
    Task(#[from] task::TaskError),
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
    Inspect(#[from] inspect::InspectError),
    #[error(transparent)]
    Watch(#[from] watch::WatchCommandError),
    #[error(transparent)]
    Cache(#[from] cache::CacheError),
}

impl RunError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Run(error) => error.exit_code(),
            Self::Plan(error) => error.exit_code(),
            Self::Task(error) => error.exit_code(),
            Self::Select(error) => error.exit_code(),
            Self::Doctor(error) => error.exit_code(),
            Self::Completion(_) => completion::CompletionError::exit_code(),
            Self::Complete(_) => exit::SUCCESS,
            Self::Manpage(_) => manpage::ManpageError::exit_code(),
            Self::Graph(error) => error.exit_code(),
            Self::Inspect(error) => error.exit_code(),
            Self::Watch(error) => error.exit_code(),
            Self::Cache(error) => error.exit_code(),
            Self::MissingAppName | Self::Usage(_) | Self::FlakeAppRef(_) => exit::USAGE,
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
    let nix_flags = nix_flags_from_cli(cli).map_err(RunError::Usage)?;
    match &cli.command {
        None if cli.select => run_with_selected_app(cli, &nix_flags, &[], runner),
        None => run_list(cli, &nix_flags, None, runner),
        Some(Command::List { category }) => run_list(cli, &nix_flags, category.as_deref(), runner),
        Some(Command::Select) => run_with_selected_app(cli, &nix_flags, &[], runner),
        Some(Command::Run {
            app,
            watch,
            debounce,
            args,
        }) => dispatch_run_command(cli, &nix_flags, app, *watch, *debounce, args, runner),
        Some(Command::Plan { app, args }) => {
            let request = app_request(cli, &nix_flags, app, args)?;
            plan::run(&request, cli.json, runner)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Task {
            jobs,
            keep_going,
            watch,
            debounce,
            task: name,
            args,
        }) => {
            if *watch {
                execute_watch(
                    cli,
                    &nix_flags,
                    name,
                    args,
                    watch_options_from_debounce(*debounce),
                    runner,
                )
            } else {
                let request = task_request(cli, &nix_flags, name, args, *jobs, *keep_going)?;
                task::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Doctor {
            clean_env,
            all,
            app,
        }) => dispatch_doctor(cli, *clean_env, *all, app.as_deref(), runner),
        Some(Command::External(tokens)) => dispatch_external(cli, &nix_flags, tokens, runner),
        Some(Command::Completion { shell }) => {
            completion::run(*shell)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Complete { target }) => {
            complete::run(
                *target,
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                cli.refresh_discovery,
                &nix_flags,
            )?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Manpage) => {
            manpage::run()?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Inspect { category, target }) => {
            run_inspect(cli, &nix_flags, category.as_deref(), target.as_ref(), runner)
        }
        Some(Command::Watch {
            name,
            debounce,
            include,
            exclude,
            clear,
            args,
        }) => execute_watch(
            cli,
            &nix_flags,
            name,
            args,
            watch::WatchOptions::from_cli(*debounce, include, exclude, *clear),
            runner,
        ),
        Some(Command::Graph { task, format }) => {
            let request = graph::GraphRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
                task: task.as_str(),
                nix_flags: &nix_flags,
            };
            graph::run(&request, *format, cli.json, runner)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Cache { action }) => match action {
            CacheSubcommand::Clear => {
                cache::clear(cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
            CacheSubcommand::Status => {
                cache::status(cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
        },
    }
}

fn run_list(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    category: Option<&str>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    list::run(
        cli.flake.as_deref(),
        cli.nix.as_deref(),
        cli.json,
        cli.refresh_discovery,
        nix_flags,
        category,
        runner,
    )?;
    Ok(exit::SUCCESS)
}

fn run_inspect(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    category: Option<&str>,
    target: Option<&InspectSubcommand>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let inspect_target = match target {
        None => inspect::InspectTarget::Overview,
        Some(InspectSubcommand::App { name }) => inspect::InspectTarget::App { name: name.clone() },
        Some(InspectSubcommand::Task { name }) => {
            inspect::InspectTarget::Task { name: name.clone() }
        }
    };
    inspect::run(
        inspect::InspectRequest {
            flake_arg: cli.flake.as_deref(),
            nix_override: cli.nix.as_deref(),
            target: inspect_target,
            category,
        },
        cli.json,
        cli.refresh_discovery,
        nix_flags,
        runner,
    )?;
    Ok(exit::SUCCESS)
}

fn run_with_selected_app(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let app = select::pick_app_name(discover_request(cli, nix_flags))?;
    let request = app_request(cli, nix_flags, &app, args)?;
    run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

fn discover_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
) -> DiscoverRequest<'a> {
    DiscoverRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        nix_flags,
    }
}

fn app_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
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
        shell: cli.dev_shell.as_deref(),
        environment_policy: environment_policy_from_cli(cli)?,
        nix_flags,
    })
}

fn task_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    task: &'a str,
    args: &'a [String],
    jobs: usize,
    keep_going: bool,
) -> Result<task::TaskRequest<'a>, RunError> {
    Ok(task::TaskRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        task,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        environment_policy: environment_policy_from_cli(cli)?,
        jobs,
        keep_going,
        output_mode: cli.output,
        events_format: cli.events,
        nix_flags,
    })
}

fn dispatch_doctor(
    cli: &Cli,
    clean_env: bool,
    all: bool,
    app: Option<&str>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let (flake_arg, app) = resolve_doctor_app(cli, app)?;
    let request = doctor::DoctorRequest {
        flake_arg,
        nix_override: cli.nix.as_deref(),
        app,
        clean_env: clean_env || cli.clean_env,
        all,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
    };
    doctor::run(request, cli.json, runner).map_err(RunError::from)
}

fn dispatch_external(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    tokens: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let (app, forwarded) = split_external(tokens)?;
    if cli.select {
        run_with_selected_app(cli, nix_flags, forwarded, runner)
    } else {
        let request = app_request(cli, nix_flags, app, forwarded)?;
        run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
    }
}

fn dispatch_run_command(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    app: &str,
    watch: bool,
    debounce: Option<u64>,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    if watch {
        return execute_watch(
            cli,
            nix_flags,
            app,
            args,
            watch_options_from_debounce(debounce),
            runner,
        );
    }
    if cli.select {
        return run_with_selected_app(cli, nix_flags, args, runner);
    }
    let request = app_request(cli, nix_flags, app, args)?;
    run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

fn execute_watch(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    name: &str,
    args: &[String],
    options: watch::WatchOptions,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = watch_request(cli, nix_flags, name, args, options)?;
    watch::run(&request, runner)?;
    Ok(exit::SUCCESS)
}

fn watch_options_from_debounce(debounce: Option<u64>) -> watch::WatchOptions {
    let mut options = watch::WatchOptions::default();
    if let Some(ms) = debounce {
        options.debounce = std::time::Duration::from_millis(ms);
    }
    options
}

fn watch_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    name: &'a str,
    args: &'a [String],
    options: watch::WatchOptions,
) -> Result<watch::WatchRequest<'a>, RunError> {
    Ok(watch::WatchRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        environment_policy: environment_policy_from_cli(cli)?,
        options,
        nix_flags,
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
