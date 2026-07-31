//! `nxr` CLI entrypoint.

mod cli;
mod commands;
mod error_format;
mod flake;
mod lean;
mod log_dir;
mod nix_flags;
mod osc52;
mod output;
mod output_options;
mod output_task;
mod reports;
mod runner_output;
mod shell_mode;
mod tui;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process;

use clap::Parser;
use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, parse_env_name, parse_set_env};
use nxr_core::{emit_stderr, perf_enabled};

use crate::cli::{
    BuildSubcommand, CacheInvalidateTarget, CacheSubcommand, CiSubcommand, Cli, Command,
    ContextSubcommand, DaemonSubcommand, DoctorSubcommand, ExplainSubcommand, HistorySubcommand,
    InspectSubcommand, MigrateSubcommand, TrustSubcommand,
};
use crate::commands::common::{AppRequest, DiscoverRequest};
use crate::commands::{
    affected, attach, cache, ci, complete, completion, configurations, context, daemon, doctor,
    doctor_builders, doctor_cache, doctor_determinate, doctor_env, envrc, explain, fmt, graph,
    history, init, inspect, inventory, list, manpage, migrate, nix_op, plan, process_cmd, run,
    script, select, selectors, task, trust, watch,
};
use crate::error_format::format_error_message;
use crate::flake::{ParseFlakeAppRefError, parse_flake_app_ref};
use crate::nix_flags::nix_flags_from_cli;
use crate::output_options::OutputOptions;
use crate::reports::{ReportKind, ReportPaths, parse_report_spec};
use crate::runner_output::RunnerOutput;

fn main() {
    if let Some(result) = lean::try_run() {
        match result {
            Ok(code) => process::exit(code),
            Err(message) => {
                eprintln!("error: {message}");
                process::exit(nxr_core::diagnostics::exit::USAGE);
            }
        }
    }

    let cli = Cli::parse();
    let output = output_options_from_cli(&cli);
    let runner = RunnerOutput::new(output);
    let result = dispatch(&cli, runner);

    match result {
        Ok(code) => {
            if perf_enabled() {
                let _ = emit_stderr();
            }
            process::exit(code)
        }
        Err(error) => {
            let _ = runner.error(format_error_message(&error));
            if perf_enabled() {
                let _ = emit_stderr();
            }
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
    Script(#[from] script::ScriptError),
    #[error(transparent)]
    NixOp(#[from] nix_op::NixOpError),
    #[error(transparent)]
    Plan(#[from] plan::PlanError),
    #[error(transparent)]
    Task(#[from] task::TaskError),
    #[error(transparent)]
    Select(#[from] select::SelectError),
    #[error(transparent)]
    Doctor(#[from] doctor::DoctorError),
    #[error(transparent)]
    DoctorDeterminate(#[from] doctor_determinate::DoctorDeterminateError),
    #[error(transparent)]
    DoctorEnv(#[from] doctor_env::DoctorEnvError),
    #[error(transparent)]
    DoctorCache(#[from] doctor_cache::DoctorCacheError),
    #[error(transparent)]
    DoctorBuilders(#[from] doctor_builders::DoctorBuildersError),
    #[error(transparent)]
    Fmt(#[from] fmt::FmtError),
    #[error(transparent)]
    Envrc(#[from] envrc::EnvrcError),
    #[error(transparent)]
    Init(#[from] init::InitError),
    #[error(transparent)]
    Migrate(#[from] migrate::MigrateError),
    #[error(transparent)]
    Configuration(#[from] configurations::ConfigurationError),
    #[error(transparent)]
    Explain(#[from] explain::ExplainError),
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
    #[error(transparent)]
    Daemon(#[from] daemon::DaemonCommandError),
    #[error(transparent)]
    History(#[from] history::HistoryError),
    #[error(transparent)]
    Attach(#[from] attach::AttachError),
    #[error(transparent)]
    Affected(#[from] affected::AffectedCommandError),
    #[error(transparent)]
    Ci(#[from] ci::CiPlanError),
    #[error(transparent)]
    Selector(#[from] selectors::SelectorCommandError),
    #[error(transparent)]
    Context(#[from] context::ContextCommandError),
    #[error(transparent)]
    Trust(#[from] trust::TrustCommandError),
    #[error(transparent)]
    Inventory(#[from] inventory::InventoryError),
    #[error(transparent)]
    Process(#[from] process_cmd::ProcessError),
}

impl RunError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Run(error) => error.exit_code(),
            Self::Script(error) => error.exit_code(),
            Self::NixOp(error) => error.exit_code(),
            Self::Plan(error) => error.exit_code(),
            Self::Task(error) => error.exit_code(),
            Self::Select(error) => error.exit_code(),
            Self::Doctor(error) => error.exit_code(),
            Self::DoctorDeterminate(error) => error.exit_code(),
            Self::DoctorEnv(error) => error.exit_code(),
            Self::DoctorCache(error) => error.exit_code(),
            Self::DoctorBuilders(error) => error.exit_code(),
            Self::Fmt(error) => error.exit_code(),
            Self::Envrc(error) => error.exit_code(),
            Self::Init(error) => error.exit_code(),
            Self::Migrate(error) => error.exit_code(),
            Self::Configuration(error) => error.exit_code(),
            Self::Explain(error) => error.exit_code(),
            Self::Completion(_) => completion::CompletionError::exit_code(),
            Self::Complete(_) => exit::SUCCESS,
            Self::Manpage(_) => manpage::ManpageError::exit_code(),
            Self::Graph(error) => error.exit_code(),
            Self::Inspect(error) => error.exit_code(),
            Self::Watch(error) => error.exit_code(),
            Self::Cache(error) => error.exit_code(),
            Self::Daemon(error) => error.exit_code(),
            Self::History(error) => error.exit_code(),
            Self::Attach(error) => error.exit_code(),
            Self::Affected(error) => error.exit_code(),
            Self::Ci(error) => error.exit_code(),
            Self::Selector(error) => error.exit_code(),
            Self::Context(error) => error.exit_code(),
            Self::Trust(error) => error.exit_code(),
            Self::Inventory(error) => error.exit_code(),
            Self::Process(error) => error.exit_code(),
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

#[allow(clippy::too_many_lines)]
fn dispatch(cli: &Cli, runner: RunnerOutput) -> Result<i32, RunError> {
    let nix_flags = nix_flags_from_cli(cli).map_err(RunError::Usage)?;
    match &cli.command {
        None if cli.select => run_with_selected_app(cli, &nix_flags, &[], runner),
        None => run_list(
            cli,
            &nix_flags,
            None,
            None,
            None,
            &affected::AffectedPathSources::default(),
            &[],
            runner,
        ),
        Some(Command::List {
            filter,
            category,
            namespace,
            base,
            working_tree,
            all_changes,
            paths,
        }) => run_list(
            cli,
            &nix_flags,
            filter.as_deref(),
            category.as_deref(),
            namespace.as_deref(),
            &affected::AffectedPathSources {
                base: base.clone(),
                working_tree: *working_tree,
                all_changes: all_changes.clone(),
            },
            paths,
            runner,
        ),
        Some(Command::Select) => run_with_selected_app(cli, &nix_flags, &[], runner),
        Some(Command::Run {
            app,
            watch,
            debounce,
            args,
        }) => dispatch_run_command(cli, &nix_flags, app, *watch, *debounce, args, runner),
        Some(Command::Script {
            list,
            path_or_name,
            args,
        }) => {
            if *list {
                let entries =
                    script::list_convention_scripts(cli.flake.as_deref(), cli.nix.as_deref())
                        .map_err(RunError::from)?;
                script::write_convention_script_list(&entries, cli.json).map_err(RunError::from)?;
                Ok(exit::SUCCESS)
            } else {
                let path_or_name = path_or_name.as_deref().ok_or_else(|| {
                    RunError::Usage("missing script path or convention name".to_owned())
                })?;
                let request = script_request(cli, &nix_flags, path_or_name, args)?;
                script::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Build {
            target,
            installable,
            attr,
        }) => dispatch_build(
            cli,
            &nix_flags,
            target.as_ref(),
            installable.as_deref(),
            attr.as_deref(),
            runner,
        ),
        Some(Command::Check { name }) => {
            dispatch_nix_op(cli, &nix_flags, name.as_deref(), NixOp::Check, runner)
        }
        Some(Command::Shell { name }) => {
            dispatch_nix_op(cli, &nix_flags, name.as_deref(), NixOp::Shell, runner)
        }
        Some(Command::Plan {
            app,
            affected,
            base,
            working_tree,
            all_changes,
            strict: _,
            no_strict,
            paths,
            args,
        }) => {
            let mut tokens = Vec::new();
            if let Some(name) = app {
                tokens.push(name.clone());
            }
            let use_affected = *affected || selectors::tokens_request_affected(&tokens);
            if use_affected {
                let requested = if tokens.is_empty() {
                    Vec::new()
                } else {
                    selectors::expand_task_tokens(
                        cli.flake.as_deref(),
                        cli.nix.as_deref(),
                        cli.refresh_discovery,
                        &nix_flags,
                        &tokens,
                    )
                    .map_err(RunError::from)?
                    .tasks
                };
                dispatch_plan_affected(
                    cli,
                    &nix_flags,
                    requested,
                    *no_strict,
                    &affected::AffectedPathSources {
                        base: base.clone(),
                        working_tree: *working_tree,
                        all_changes: all_changes.clone(),
                    },
                    paths,
                    args,
                    runner,
                )
            } else if tokens.is_empty() {
                Err(RunError::Usage(
                    "plan requires an app, task, or selector (or --affected / changed)".to_owned(),
                ))
            } else if tokens.len() == 1 && !selectors::token_is_selector(&tokens[0]) {
                // Bare name: resolve app first (then task) inside plan::run. Do not
                // expand as task selectors here — that rejects app-only flakes.
                dispatch_plan(cli, &nix_flags, &tokens[0], args, runner)
            } else {
                let resolved = selectors::expand_task_tokens(
                    cli.flake.as_deref(),
                    cli.nix.as_deref(),
                    cli.refresh_discovery,
                    &nix_flags,
                    &tokens,
                )
                .map_err(RunError::from)?;
                if resolved.tasks.is_empty() {
                    return Err(RunError::Usage(
                        "plan requires an app, task, or selector".to_owned(),
                    ));
                }
                // Explicit selectors always plan as tasks (never app-first).
                let used_selector = tokens
                    .iter()
                    .any(|token| selectors::token_is_selector(token));
                if resolved.tasks.len() == 1 && !used_selector {
                    dispatch_plan(cli, &nix_flags, &resolved.tasks[0], args, runner)
                } else {
                    dispatch_plan_multi(cli, &nix_flags, &resolved.tasks, runner)
                }
            }
        }
        Some(Command::Task {
            jobs,
            keep_going,
            watch,
            debounce,
            affected,
            base,
            working_tree,
            all_changes,
            strict: _,
            no_strict,
            paths,
            tasks,
            junit,
            sarif,
            coverage,
            benchmark,
            set,
            args,
        }) => {
            let report_options = TaskReportOptions {
                junit: junit.clone(),
                sarif: sarif.clone(),
                coverage: coverage.clone(),
                benchmark: benchmark.clone(),
            };
            let param_sets =
                crate::commands::task_params::parse_param_sets(set).map_err(RunError::Usage)?;
            let use_affected = *affected || selectors::tokens_request_affected(tasks);
            if use_affected {
                let requested = if tasks.is_empty() {
                    Vec::new()
                } else {
                    selectors::expand_task_tokens(
                        cli.flake.as_deref(),
                        cli.nix.as_deref(),
                        cli.refresh_discovery,
                        &nix_flags,
                        tasks,
                    )
                    .map_err(RunError::from)?
                    .tasks
                };
                dispatch_task_affected(
                    cli,
                    &nix_flags,
                    &requested,
                    args,
                    *jobs,
                    *keep_going,
                    !*no_strict,
                    &affected::AffectedPathSources {
                        base: base.clone(),
                        working_tree: *working_tree,
                        all_changes: all_changes.clone(),
                    },
                    paths,
                    &report_options,
                    param_sets,
                    runner,
                )
            } else if *watch {
                let options = watch_options_from_debounce(*debounce);
                let request = watch_task_request(
                    cli,
                    &nix_flags,
                    tasks,
                    args,
                    *jobs,
                    *keep_going,
                    options,
                    &report_options,
                    param_sets,
                )?;
                watch::run(&request, runner).map_err(RunError::from)
            } else if tasks
                .iter()
                .all(|token| !selectors::token_is_selector(token))
            {
                // Bare task names/aliases: resolve inside task::execute so unknown
                // roots keep NOT_FOUND + `unknown task root` (not selector USAGE).
                if tasks.is_empty() {
                    return Err(RunError::Usage(
                        "task requires a name or selector (or --affected / changed)".to_owned(),
                    ));
                }
                let request = task_request(
                    cli,
                    &nix_flags,
                    tasks.clone(),
                    args,
                    *jobs,
                    *keep_going,
                    &report_options,
                    param_sets,
                )?;
                task::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            } else {
                let resolved = selectors::expand_task_tokens(
                    cli.flake.as_deref(),
                    cli.nix.as_deref(),
                    cli.refresh_discovery,
                    &nix_flags,
                    tasks,
                )
                .map_err(RunError::from)?;
                if resolved.tasks.is_empty() {
                    return Err(RunError::Usage(
                        "task requires a name or selector (or --affected / changed)".to_owned(),
                    ));
                }
                let request = task_request(
                    cli,
                    &nix_flags,
                    resolved.tasks,
                    args,
                    *jobs,
                    *keep_going,
                    &report_options,
                    param_sets,
                )?;
                task::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
            }
        }
        Some(Command::Doctor {
            target,
            clean_env,
            all,
            app,
        }) => {
            if let Some(sub) = target.as_ref() {
                return dispatch_doctor_subcommand(cli, sub, runner);
            }
            dispatch_doctor(cli, *clean_env, *all, app.as_deref(), runner)
        }
        Some(Command::Explain { name, target, args }) => dispatch_explain(
            cli,
            &nix_flags,
            name.as_deref(),
            target.as_ref(),
            args,
            runner,
        ),
        Some(Command::External(tokens)) => dispatch_external(cli, &nix_flags, tokens, runner),
        Some(Command::Completion { shell }) => {
            completion::run(*shell)?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Complete {
            target,
            task,
            parameter,
        }) => {
            complete::run(
                *target,
                task.as_deref(),
                parameter.as_deref(),
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
        Some(Command::Inspect {
            category,
            namespace,
            target,
        }) => run_inspect(
            cli,
            &nix_flags,
            category.as_deref(),
            namespace.as_deref(),
            target.as_ref(),
            runner,
        ),
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
            false,
            runner,
        ),
        Some(Command::Graph { task, format }) => {
            dispatch_graph(cli, &nix_flags, task, *format, runner)
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
            CacheSubcommand::Gc => {
                cache::gc(cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
            CacheSubcommand::Invalidate { target } => match target {
                CacheInvalidateTarget::Discovery { file_stem, key } => {
                    cache::invalidate_discovery(
                        file_stem.as_deref(),
                        key.as_deref(),
                        cli.json,
                        runner,
                    )?;
                    Ok(exit::SUCCESS)
                }
                CacheInvalidateTarget::Plan { key_digest } => {
                    cache::invalidate_plan(key_digest.as_deref(), cli.json, runner)?;
                    Ok(exit::SUCCESS)
                }
                CacheInvalidateTarget::DevEnv { key_digest } => {
                    cache::invalidate_dev_env(key_digest.as_deref(), cli.json, runner)?;
                    Ok(exit::SUCCESS)
                }
            },
            CacheSubcommand::Explain { task, tasks } => {
                if let Some(task) = task {
                    cache::explain_task(
                        cli.flake.as_deref(),
                        cli.nix.as_deref(),
                        task,
                        cli.json,
                        runner,
                    )?;
                } else {
                    cache::explain(
                        cli.flake.as_deref(),
                        cli.nix.as_deref(),
                        *tasks,
                        cli.json,
                        &nix_flags,
                        runner,
                    )?;
                }
                Ok(exit::SUCCESS)
            }
        },
        Some(Command::Daemon { action }) => match action {
            DaemonSubcommand::Start { foreground, socket } => Ok(daemon::start(
                socket.as_ref().map(PathBuf::from),
                *foreground,
                cli.json,
                runner,
            )?),
            DaemonSubcommand::Stop { socket } => Ok(daemon::stop(
                socket.as_ref().map(PathBuf::from),
                cli.json,
                runner,
            )?),
            DaemonSubcommand::Status { socket } => Ok(daemon::status(
                socket.as_ref().map(PathBuf::from),
                cli.json,
                runner,
            )?),
        },
        Some(Command::History { action }) => match action {
            None | Some(HistorySubcommand::List) => {
                history::list(cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
            Some(HistorySubcommand::Clear) => {
                history::clear(cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
        },
        Some(Command::Attach { run }) => {
            attach::run(run.as_deref(), runner)?;
            Ok(exit::SUCCESS)
        },
        Some(Command::Trust { action }) => match action {
            TrustSubcommand::Status => {
                trust::status(cli.flake.as_deref(), cli.json, runner)?;
                Ok(exit::SUCCESS)
            }
            TrustSubcommand::Add => {
                trust::add(cli.flake.as_deref(), runner)?;
                Ok(exit::SUCCESS)
            }
            TrustSubcommand::Revoke => {
                trust::revoke(cli.flake.as_deref(), runner)?;
                Ok(exit::SUCCESS)
            }
        },
        Some(Command::Inventory { role }) => {
            inventory::run(
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                role.as_deref(),
                cli.json,
                &nix_flags,
                runner,
            )?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Up { names }) => Ok(process_cmd::up(
            cli.flake.as_deref(),
            cli.nix.as_deref(),
            names,
            &nix_flags,
            runner,
        )?),
        Some(Command::Status) => Ok(process_cmd::status(
            cli.flake.as_deref(),
            cli.nix.as_deref(),
            cli.json,
            &nix_flags,
            runner,
        )?),
        Some(Command::Logs { name, follow }) => Ok(process_cmd::logs(
            cli.flake.as_deref(),
            cli.nix.as_deref(),
            name,
            *follow,
            &nix_flags,
            runner,
        )?),
        Some(Command::Down { names }) => Ok(process_cmd::down(
            cli.flake.as_deref(),
            cli.nix.as_deref(),
            names,
            &nix_flags,
            runner,
        )?),
        Some(Command::Affected {
            base,
            working_tree,
            all_changes,
            strict: _,
            no_strict,
            paths,
        }) => {
            // Default is strict (include unknown). Only `--no-strict` opts out.
            let strict_policy = !*no_strict;
            affected::run(
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                cli.json,
                cli.refresh_discovery,
                &nix_flags,
                &affected::AffectedPathSources {
                    base: base.clone(),
                    working_tree: *working_tree,
                    all_changes: all_changes.clone(),
                },
                strict_policy,
                paths,
                runner,
            )?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Fmt { paths }) => {
            let request = fmt::FmtRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
                paths,
                dry_run: cli.dry_run,
                json: cli.json,
                nix_flags: &nix_flags,
            };
            fmt::run(&request, runner).map_err(RunError::from)
        }
        Some(Command::Envrc { write, force }) => {
            let request = envrc::EnvrcRequest {
                flake_arg: cli.flake.as_deref(),
                shell: cli.dev_shell.as_deref(),
                write: *write,
                force: *force,
            };
            envrc::run(request, runner).map_err(RunError::from)
        }
        Some(Command::Init {
            template,
            template_long,
            dir,
            yes,
        }) => {
            let request = init::InitRequest {
                template: template.as_deref().or(template_long.as_deref()),
                target_dir: dir.as_deref().map(camino::Utf8Path::new),
                yes: *yes,
            };
            init::run(request, runner).map_err(RunError::from)
        }
        Some(Command::Context { action }) => dispatch_context(cli, &nix_flags, action, runner),
        Some(Command::Migrate { source }) => dispatch_migrate(source, runner),
        Some(Command::In { shell, verb, rest }) => {
            dispatch_in(cli, &nix_flags, shell, verb, rest, runner)
        }
        Some(Command::Ci {
            action:
                CiSubcommand::Plan {
                    roots,
                    base,
                    working_tree,
                    all_changes,
                    strict: _,
                    no_strict,
                    paths,
                },
        }) => {
            let strict_policy = !*no_strict;
            let request = ci::CiPlanRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
                refresh_discovery: cli.refresh_discovery,
                json: cli.json,
                nix_flags: &nix_flags,
                path_sources: &affected::AffectedPathSources {
                    base: base.clone(),
                    working_tree: *working_tree,
                    all_changes: all_changes.clone(),
                },
                strict: strict_policy,
                paths,
                roots,
            };
            ci::plan_run(&request, runner).map_err(RunError::from)?;
            Ok(exit::SUCCESS)
        }
    }
}

fn dispatch_plan(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    app: &str,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = app_request(cli, nix_flags, app, args)?;
    plan::run(&request, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_plan_affected(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    requested: Vec<String>,
    no_strict: bool,
    sources: &affected::AffectedPathSources,
    paths: &[String],
    _args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let strict_policy = !no_strict;
    let selection = affected::select_for_flake(
        cli.flake.as_deref(),
        cli.nix.as_deref(),
        cli.refresh_discovery,
        nix_flags,
        sources,
        strict_policy,
        paths,
        runner,
    )?;
    let roots = affected::resolve_affected_task_roots(
        &selection.document,
        &selection.analysis,
        &requested,
    )?;
    plan::run_affected_tasks(&selection.document, &roots, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TaskReportOptions {
    junit: Option<String>,
    sarif: Option<String>,
    coverage: Option<String>,
    benchmark: Option<String>,
}

fn report_paths_from_cli(
    cli: &Cli,
    task_reports: &TaskReportOptions,
) -> Result<ReportPaths, RunError> {
    let mut paths = ReportPaths::default();
    for spec in &cli.report {
        let (kind, path) =
            parse_report_spec(spec).map_err(|error| RunError::Usage(error.to_string()))?;
        paths.set(kind, path);
    }
    if let Some(path) = &task_reports.junit {
        paths.set(ReportKind::Junit, PathBuf::from(path));
    }
    if let Some(path) = &task_reports.sarif {
        paths.set(ReportKind::Sarif, PathBuf::from(path));
    }
    if let Some(path) = &task_reports.coverage {
        paths.set(ReportKind::Coverage, PathBuf::from(path));
    }
    if let Some(path) = &task_reports.benchmark {
        paths.set(ReportKind::Benchmark, PathBuf::from(path));
    }
    Ok(paths)
}

fn dispatch_plan_multi(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    roots: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let document = selectors::load_task_document(
        cli.flake.as_deref(),
        cli.nix.as_deref(),
        cli.refresh_discovery,
        nix_flags,
    )
    .map_err(RunError::from)?;
    plan::run_affected_tasks(&document, roots, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_task_affected(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    requested: &[String],
    args: &[String],
    jobs: usize,
    keep_going: bool,
    strict: bool,
    sources: &affected::AffectedPathSources,
    paths: &[String],
    report_options: &TaskReportOptions,
    param_sets: BTreeMap<String, String>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let selection = affected::select_for_flake(
        cli.flake.as_deref(),
        cli.nix.as_deref(),
        cli.refresh_discovery,
        nix_flags,
        sources,
        strict,
        paths,
        runner,
    )?;
    let roots =
        affected::resolve_affected_task_roots(&selection.document, &selection.analysis, requested)?;
    if roots.is_empty() {
        let _ = runner.info("no affected tasks to run");
        return Ok(exit::SUCCESS);
    }
    let _ = runner.info(format!("running affected tasks: {}", roots.join(", ")));
    let request = task_request(
        cli,
        nix_flags,
        roots,
        args,
        jobs,
        keep_going,
        report_options,
        param_sets,
    )?;
    task::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NixOp {
    Check,
    Shell,
}

fn dispatch_build(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    target: Option<&BuildSubcommand>,
    installable: Option<&str>,
    attr: Option<&str>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let environment = environment_policy_from_cli(cli)?;
    let request = nix_op::NixOpRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name: installable,
        attr,
        dry_run: cli.dry_run,
        json: cli.json,
        nix_flags,
        environment: &environment,
    };

    if let Some(BuildSubcommand::Configuration { name }) = target {
        if installable.is_some() || attr.is_some() {
            return Err(RunError::Usage(
                "cannot combine `build configuration` with an installable or --attr".to_owned(),
            ));
        }
        return nix_op::execute_build_configuration(&request, name, runner).map_err(RunError::from);
    }

    if target.is_some() {
        return Err(RunError::Usage("unknown build subcommand".to_owned()));
    }

    nix_op::execute_build(&request, runner).map_err(RunError::from)
}

fn dispatch_doctor_subcommand(
    cli: &Cli,
    sub: &DoctorSubcommand,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    match sub {
        DoctorSubcommand::Determinate { all, refresh } => {
            let request = doctor_determinate::DoctorDeterminateRequest {
                nix_override: cli.nix.as_deref(),
                all: *all,
                refresh: *refresh,
            };
            doctor_determinate::run(request, cli.json, runner).map_err(RunError::from)
        }
        DoctorSubcommand::Env => {
            let request = doctor_env::DoctorEnvRequest {
                flake_arg: cli.flake.as_deref(),
            };
            doctor_env::run(request, cli.json, runner).map_err(RunError::from)
        }
        DoctorSubcommand::Cache => {
            let request = doctor_cache::DoctorCacheRequest {
                flake_arg: cli.flake.as_deref(),
                nix_override: cli.nix.as_deref(),
            };
            doctor_cache::run(request, cli.json, runner).map_err(RunError::from)
        }
        DoctorSubcommand::Builders => {
            let request = doctor_builders::DoctorBuildersRequest {
                nix_override: cli.nix.as_deref(),
            };
            doctor_builders::run(request, cli.json, runner).map_err(RunError::from)
        }
    }
}

fn dispatch_migrate(source: &MigrateSubcommand, runner: RunnerOutput) -> Result<i32, RunError> {
    let (migrate_source, path, write, file_backed, scripts) = match source {
        MigrateSubcommand::Justfile {
            path,
            write,
            file_backed,
            scripts,
        } => (
            migrate::MigrateSource::Justfile,
            path.as_deref(),
            write.as_deref(),
            *file_backed,
            *scripts,
        ),
        MigrateSubcommand::Mise {
            path,
            write,
            file_backed,
            scripts,
        } => (
            migrate::MigrateSource::Mise,
            path.as_deref(),
            write.as_deref(),
            *file_backed,
            *scripts,
        ),
    };
    let emit_options = migrate::MigrateEmitOptions {
        style: if file_backed {
            migrate::MigrateEmitStyle::FileBacked
        } else {
            migrate::MigrateEmitStyle::InlineScript
        },
        emit_scripts: scripts,
    };
    let request = migrate::MigrateRequest {
        source: migrate_source,
        input: path.map(camino::Utf8Path::new),
        write: write.map(camino::Utf8Path::new),
        emit_options,
    };
    migrate::run(request, runner).map_err(RunError::from)
}

fn dispatch_in(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    shell: &str,
    verb: &str,
    rest: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    match verb {
        "run" => {
            let app = rest.first().ok_or_else(|| {
                RunError::Usage("missing app name after `in <shell> run`".to_owned())
            })?;
            let args = &rest[1..];
            dispatch_run_command_in_shell(cli, nix_flags, shell, app, false, None, args, runner)
        }
        "script" => {
            let path_or_name = rest.first().ok_or_else(|| {
                RunError::Usage("missing path or name after `in <shell> script`".to_owned())
            })?;
            let args = &rest[1..];
            let request = script_request_in_shell(cli, nix_flags, shell, path_or_name, args)?;
            script::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
        }
        "plan" => {
            let app = rest.first().ok_or_else(|| {
                RunError::Usage("missing app or task name after `in <shell> plan`".to_owned())
            })?;
            let args = &rest[1..];
            dispatch_plan_in_shell(cli, nix_flags, shell, app, args, runner)
        }
        "task" => {
            if rest.is_empty() {
                return Err(RunError::Usage(
                    "missing task name after `in <shell> task`".to_owned(),
                ));
            }
            let request =
                task_request_in_shell(cli, nix_flags, shell, rest.to_vec(), &[], 1, false)?;
            task::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
        }
        "watch" => {
            let name = rest.first().ok_or_else(|| {
                RunError::Usage("missing name after `in <shell> watch`".to_owned())
            })?;
            let args = &rest[1..];
            let request = watch_request_in_shell(
                cli,
                nix_flags,
                shell,
                name,
                args,
                watch::WatchOptions::default(),
                false,
            )?;
            watch::run(&request, runner).map_err(RunError::from)
        }
        "explain" => {
            let name = rest.first().ok_or_else(|| {
                RunError::Usage("missing name after `in <shell> explain`".to_owned())
            })?;
            let args = &rest[1..];
            dispatch_explain_in_shell(cli, nix_flags, shell, name, args, runner)
        }
        app => dispatch_run_command_in_shell(cli, nix_flags, shell, app, false, None, rest, runner),
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_run_command_in_shell(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    shell: &str,
    app: &str,
    watch: bool,
    debounce: Option<u64>,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    if watch {
        let request = watch_request_in_shell(
            cli,
            nix_flags,
            shell,
            app,
            args,
            watch_options_from_debounce(debounce),
            true,
        )?;
        return watch::run(&request, runner).map_err(RunError::from);
    }
    let request = app_request_in_shell(cli, nix_flags, shell, app, args)?;
    run::execute(&request, cli.dry_run, cli.json, runner).map_err(RunError::from)
}

fn dispatch_plan_in_shell(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    shell: &str,
    app: &str,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = app_request_in_shell(cli, nix_flags, shell, app, args)?;
    plan::run(&request, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

fn dispatch_explain_in_shell(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    shell: &str,
    name: &str,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = explain::ExplainRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        kind: None,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: Some(shell),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        jobs: 1,
        output_mode: cli.output,
        events_format: cli.events,
        nix_flags,
    };
    explain::run(&request, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

fn app_request_in_shell<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    shell: &'a str,
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
        shell: Some(shell),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        nix_flags,
        context: cli.execution_context.as_deref(),
    })
}

fn task_request_in_shell<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    shell: &'a str,
    tasks: Vec<String>,
    args: &'a [String],
    jobs: usize,
    keep_going: bool,
) -> Result<task::TaskRequest<'a>, RunError> {
    Ok(task::TaskRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        tasks,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: Some(shell),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        jobs,
        keep_going,
        output_mode: cli.output,
        events_format: cli.events,
        reports: report_paths_from_cli(cli, &TaskReportOptions::default())?,
        nix_flags,
        context_override: None,
        refresh_discovery: cli.refresh_discovery,
        param_sets: BTreeMap::new(),
        log_dir: cli.log_dir.clone(),
    })
}

fn watch_request_in_shell<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    shell: &'a str,
    name: &'a str,
    args: &'a [String],
    options: watch::WatchOptions,
    force_app: bool,
) -> Result<watch::WatchRequest<'a>, RunError> {
    Ok(watch::WatchRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: Some(shell),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        options,
        output_mode: cli.output,
        events_format: cli.events,
        task_settings: None,
        force_app,
        nix_flags,
    })
}

fn dispatch_nix_op(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    name: Option<&str>,
    op: NixOp,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let environment = environment_policy_from_cli(cli)?;
    let request = nix_op::NixOpRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        attr: None,
        dry_run: cli.dry_run,
        json: cli.json,
        nix_flags,
        environment: &environment,
    };
    match op {
        NixOp::Check => nix_op::execute_check(&request, runner).map_err(RunError::from),
        NixOp::Shell => nix_op::execute_shell(&request, runner).map_err(RunError::from),
    }
}

fn dispatch_graph(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    task: &str,
    format: graph::GraphFormat,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = graph::GraphRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        task,
        nix_flags,
    };
    graph::run(&request, format, cli.json, runner)?;
    Ok(exit::SUCCESS)
}

fn run_list(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    filter: Option<&str>,
    category: Option<&str>,
    namespace: Option<&str>,
    path_sources: &affected::AffectedPathSources,
    paths: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    list::run(
        cli.flake.as_deref(),
        cli.nix.as_deref(),
        cli.json,
        cli.refresh_discovery,
        nix_flags,
        filter,
        category,
        namespace,
        path_sources,
        paths,
        runner,
    )?;
    Ok(exit::SUCCESS)
}

fn run_inspect(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    category: Option<&str>,
    namespace: Option<&str>,
    target: Option<&InspectSubcommand>,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let inspect_target = match target {
        None => inspect::InspectTarget::Overview,
        Some(InspectSubcommand::App { name }) => inspect::InspectTarget::App { name: name.clone() },
        Some(InspectSubcommand::Task { name }) => {
            inspect::InspectTarget::Task { name: name.clone() }
        }
        Some(InspectSubcommand::Configuration { name }) => {
            configurations::inspect(
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                name,
                cli.json,
                nix_flags,
                runner,
            )?;
            return Ok(exit::SUCCESS);
        }
        Some(InspectSubcommand::Inventory { role, name }) => {
            inventory::inspect_entry(
                cli.flake.as_deref(),
                cli.nix.as_deref(),
                role,
                name,
                cli.json,
                nix_flags,
                runner,
            )?;
            return Ok(exit::SUCCESS);
        }
    };
    inspect::run(
        inspect::InspectRequest {
            flake_arg: cli.flake.as_deref(),
            nix_override: cli.nix.as_deref(),
            target: inspect_target,
            category,
            namespace,
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
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        nix_flags,
        context: cli.execution_context.as_deref(),
    })
}

fn script_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    path_or_name: &'a str,
    args: &'a [String],
) -> Result<script::ScriptRequest<'a>, RunError> {
    Ok(script::ScriptRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        path_or_name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        nix_flags,
        context: cli.execution_context.as_deref(),
    })
}

fn script_request_in_shell<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    shell: &'a str,
    path_or_name: &'a str,
    args: &'a [String],
) -> Result<script::ScriptRequest<'a>, RunError> {
    Ok(script::ScriptRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        path_or_name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: Some(shell),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        nix_flags,
        context: cli.execution_context.as_deref(),
    })
}

fn task_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    tasks: Vec<String>,
    args: &'a [String],
    jobs: usize,
    keep_going: bool,
    report_options: &TaskReportOptions,
    param_sets: BTreeMap<String, String>,
) -> Result<task::TaskRequest<'a>, RunError> {
    Ok(task::TaskRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        tasks,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        jobs,
        keep_going,
        output_mode: cli.output,
        events_format: cli.events,
        reports: report_paths_from_cli(cli, report_options)?,
        nix_flags,
        context_override: None,
        refresh_discovery: cli.refresh_discovery,
        param_sets,
        log_dir: cli.log_dir.clone(),
    })
}

fn dispatch_context(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    action: &ContextSubcommand,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let context_action = match action {
        ContextSubcommand::List => context::ContextAction::List,
        ContextSubcommand::Inspect { name } => {
            context::ContextAction::Inspect { name: name.clone() }
        }
        ContextSubcommand::Run { context, command } => context::ContextAction::Run {
            context: context.clone(),
            command: command.clone(),
        },
    };
    let report_options = TaskReportOptions::default();
    let request = context::ContextRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        refresh_discovery: cli.refresh_discovery,
        json: cli.json,
        action: context_action,
        environment_policy: environment_policy_from_cli(cli)?,
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        nix_flags,
        jobs: 1,
        keep_going: false,
        output_mode: cli.output,
        events_format: cli.events,
        reports: report_paths_from_cli(cli, &report_options)?,
        dry_run: cli.dry_run,
    };
    context::run(&request, runner).map_err(RunError::from)
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

fn dispatch_explain(
    cli: &Cli,
    nix_flags: &nxr_nix::OptionalNixFlags,
    name: Option<&str>,
    target: Option<&ExplainSubcommand>,
    args: &[String],
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let (resolved_name, resolved_kind, resolved_args) = match (target, name) {
        (Some(ExplainSubcommand::App { name, args }), None) => (
            name.as_str(),
            Some(explain::ExplainKind::App),
            args.as_slice(),
        ),
        (Some(ExplainSubcommand::Task { name, args }), None) => (
            name.as_str(),
            Some(explain::ExplainKind::Task),
            args.as_slice(),
        ),
        (None, Some(name)) => (name, None, args),
        (Some(_), Some(_)) => {
            return Err(RunError::Usage(
                "cannot combine an explain subcommand with a bare name".to_owned(),
            ));
        }
        (None, None) => {
            return Err(RunError::Usage(
                "missing explain target (use `nxr explain <name>` or `nxr explain task <name>`)"
                    .to_owned(),
            ));
        }
    };

    let request = explain::ExplainRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name: resolved_name,
        kind: resolved_kind,
        args: resolved_args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        jobs: 1,
        output_mode: cli.output,
        events_format: cli.events,
        nix_flags,
    };
    explain::run(&request, cli.json, runner)?;
    Ok(exit::SUCCESS)
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
            true,
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
    force_app: bool,
    runner: RunnerOutput,
) -> Result<i32, RunError> {
    let request = watch_request(cli, nix_flags, name, args, options, force_app)?;
    watch::run(&request, runner).map_err(RunError::from)
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
    force_app: bool,
) -> Result<watch::WatchRequest<'a>, RunError> {
    Ok(watch::WatchRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        options,
        output_mode: cli.output,
        events_format: cli.events,
        // When `name` resolves as a task, use the normal scheduler with global
        // output/events; jobs stay at 1 unless entered via `task --watch -j`.
        task_settings: None,
        force_app,
        nix_flags,
    })
}

fn watch_task_request<'a>(
    cli: &'a Cli,
    nix_flags: &'a nxr_nix::OptionalNixFlags,
    tasks: &'a [String],
    args: &'a [String],
    jobs: usize,
    keep_going: bool,
    options: watch::WatchOptions,
    report_options: &TaskReportOptions,
    param_sets: BTreeMap<String, String>,
) -> Result<watch::WatchRequest<'a>, RunError> {
    let name = tasks
        .first()
        .ok_or(RunError::Usage("task name required".to_owned()))?;
    let reports = report_paths_from_cli(cli, report_options)?;
    Ok(watch::WatchRequest {
        flake_arg: cli.flake.as_deref(),
        nix_override: cli.nix.as_deref(),
        name,
        args,
        root: cli.root,
        cwd: cli.cwd.as_deref(),
        shell: cli.dev_shell.as_deref(),
        shell_mode: cli.shell_mode,
        environment_policy: environment_policy_from_cli(cli)?,
        options,
        output_mode: cli.output,
        events_format: cli.events,
        task_settings: Some(watch::TaskWatchSettings {
            tasks: tasks.to_vec(),
            jobs,
            keep_going,
            output_mode: cli.output,
            events_format: cli.events,
            reports,
            param_sets,
            log_dir: cli.log_dir.clone(),
        }),
        force_app: false,
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
