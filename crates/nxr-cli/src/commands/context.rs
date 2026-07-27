//! `nxr context` — list, inspect, and run with named execution contexts.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::EnvironmentPolicy;
use nxr_core::config::load_user_config;
use nxr_core::diagnostics::exit;
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_task::{
    ExecutionContext, SecretDelivery, SecretProvider, TaskDocument, apply_task_context,
    authorize_secret_refs, secret_delivery_mode, secret_provider_mode,
};
use serde::Serialize;

use crate::commands::common::{
    PrepareError, build_adapter, cold_discover_workspace, current_invocation_directory,
};
use crate::commands::secrets::project_identity;
use crate::commands::task::{TaskError, TaskRequest};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;
use crate::shell_mode::ShellMode;

/// `nxr context` subcommands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextAction {
    List,
    Inspect {
        name: String,
    },
    Run {
        context: String,
        command: Vec<String>,
    },
}

/// Inputs for `nxr context`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub refresh_discovery: bool,
    pub json: bool,
    pub action: ContextAction,
    pub environment_policy: EnvironmentPolicy,
    pub shell: Option<&'a str>,
    pub shell_mode: ShellMode,
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub nix_flags: &'a OptionalNixFlags,
    pub jobs: usize,
    pub keep_going: bool,
    pub output_mode: Option<crate::output_task::TaskOutputMode>,
    pub events_format: Option<crate::output_task::EventsFormat>,
    pub reports: crate::reports::ReportPaths,
    pub dry_run: bool,
}

/// Errors while running `nxr context`.
#[derive(Debug, thiserror::Error)]
pub enum ContextCommandError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Task(#[from] TaskError),
    #[error(transparent)]
    Context(#[from] nxr_task::ContextError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unknown context: {name}")]
    UnknownContext { name: String },
    #[error("context run requires a target (task name, `task <name>`, or `run <app>`)")]
    MissingRunTarget,
    #[error("unknown context run target")]
    InvalidRunTarget,
}

impl ContextCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Task(error) => error.exit_code(),
            Self::Context(_) => exit::EVALUATION,
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
            Self::UnknownContext { .. } | Self::MissingRunTarget | Self::InvalidRunTarget => {
                exit::USAGE
            }
        }
    }
}

/// Execute `nxr context`.
///
/// # Errors
///
/// Returns [`ContextCommandError`] when discovery, authorization, or execution fails.
pub fn run(request: &ContextRequest<'_>, runner: RunnerOutput) -> Result<i32, ContextCommandError> {
    match &request.action {
        ContextAction::List => list_contexts(request, runner),
        ContextAction::Inspect { name } => inspect_context(request, name, runner),
        ContextAction::Run { context, command } => {
            run_with_context(request, context, command, runner)
        }
    }
}

fn list_contexts(
    request: &ContextRequest<'_>,
    _runner: RunnerOutput,
) -> Result<i32, ContextCommandError> {
    let document = load_task_document(request)?;
    if request.json {
        let names: Vec<&str> = document.contexts.keys().map(String::as_str).collect();
        let payload = serde_json::json!({
            "schema_version": 1,
            "contexts": names,
        });
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &payload)?;
        writeln!(stdout)?;
        return Ok(exit::SUCCESS);
    }

    if document.contexts.is_empty() {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "no contexts defined")?;
        return Ok(exit::SUCCESS);
    }

    let mut stdout = io::stdout().lock();
    for name in document.contexts.keys() {
        writeln!(stdout, "{}", sanitize_terminal_text(name))?;
    }
    Ok(exit::SUCCESS)
}

fn inspect_context(
    request: &ContextRequest<'_>,
    name: &str,
    runner: RunnerOutput,
) -> Result<i32, ContextCommandError> {
    let document = load_task_document(request)?;
    let Some(context) = document.contexts.get(name) else {
        return Err(ContextCommandError::UnknownContext {
            name: name.to_owned(),
        });
    };

    if request.json {
        let view = ContextInspectView::from_context(name, context);
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &view)?;
        writeln!(stdout)?;
        return Ok(exit::SUCCESS);
    }

    write_human_context(&mut io::stdout().lock(), name, context)?;
    let _ = runner;
    Ok(exit::SUCCESS)
}

fn run_with_context(
    request: &ContextRequest<'_>,
    context_name: &str,
    command: &[String],
    runner: RunnerOutput,
) -> Result<i32, ContextCommandError> {
    let (flake, adapter, document) = discover_context_inputs(request)?;
    if !document.contexts.contains_key(context_name) {
        return Err(ContextCommandError::UnknownContext {
            name: context_name.to_owned(),
        });
    }

    let applied = apply_task_context(
        &document,
        "context-run",
        context_name,
        &request.environment_policy,
    )?;
    authorize_context_run(&flake, &applied.plan_secrets)?;

    let (tasks, args) = parse_run_target(command)?;
    let task_request = TaskRequest {
        flake_arg: request.flake_arg,
        nix_override: request.nix_override,
        tasks,
        args: &args,
        root: request.root,
        cwd: request.cwd,
        shell: request.shell,
        shell_mode: request.shell_mode,
        environment_policy: request.environment_policy.clone(),
        jobs: request.jobs,
        keep_going: request.keep_going,
        output_mode: request.output_mode,
        events_format: request.events_format,
        reports: request.reports.clone(),
        nix_flags: request.nix_flags,
        context_override: Some(context_name.to_owned()),
        refresh_discovery: request.refresh_discovery,
    };
    let _ = adapter;
    crate::commands::task::execute(&task_request, request.dry_run, request.json, runner)
        .map_err(ContextCommandError::from)
}

fn authorize_context_run(
    flake: &FlakeSelection,
    secrets: &[nxr_task::PlanSecretEntry],
) -> Result<(), ContextCommandError> {
    if secrets.is_empty() {
        return Ok(());
    }
    let user_config = load_user_config().map_err(|error| {
        ContextCommandError::Context(nxr_task::ContextError::Config {
            message: error.to_string(),
        })
    })?;
    if user_config.trusted_projects.is_empty() {
        return Ok(());
    }
    let flake_root = flake
        .local_root
        .as_ref()
        .map(|path| path.as_std_path().to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let project_id = project_identity(&flake_root);
    let trust_refs: Vec<String> = secrets
        .iter()
        .map(|entry| entry.reference.clone())
        .collect();
    authorize_secret_refs(&project_id, &trust_refs, &user_config.trusted_projects)
        .map_err(ContextCommandError::from)
}

fn parse_run_target(command: &[String]) -> Result<(Vec<String>, Vec<String>), ContextCommandError> {
    let Some(first) = command.first() else {
        return Err(ContextCommandError::MissingRunTarget);
    };
    match first.as_str() {
        "task" => {
            let Some(name) = command.get(1) else {
                return Err(ContextCommandError::MissingRunTarget);
            };
            Ok((vec![name.clone()], command[2..].to_vec()))
        }
        "run" => Err(ContextCommandError::InvalidRunTarget),
        name => Ok((vec![name.to_owned()], command[1..].to_vec())),
    }
}

fn load_task_document(request: &ContextRequest<'_>) -> Result<TaskDocument, ContextCommandError> {
    let (_, _, document) = discover_context_inputs(request)?;
    Ok(document)
}

fn discover_context_inputs(
    request: &ContextRequest<'_>,
) -> Result<(FlakeSelection, nxr_nix::NixAdapter, TaskDocument), ContextCommandError> {
    let invocation_directory = current_invocation_directory()?;
    let adapter = build_adapter(request.nix_override)?;
    let flake = resolve_flake(request.flake_arg, &invocation_directory)?;
    let workspace = discover_workspace(
        &flake,
        &adapter,
        request.refresh_discovery,
        request.nix_flags,
    )?;
    let document = workspace
        .tasks
        .unwrap_or_else(|| nxr_task::TaskDocument::new(std::collections::BTreeMap::new()));
    Ok((flake, adapter, document))
}

fn discover_workspace(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<WorkspaceDiscovery, ContextCommandError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };
    let flake_ref = flake.nix_ref.clone();
    discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions::with_tasks(refresh_discovery),
        || {
            // Share cold discovery with task execution so cached entries include
            // apps, tasks, and dev shells (context shell validation needs shells).
            cold_discover_workspace(adapter, &flake_ref, true, nix_flags).map(|cold| cold.discovery)
        },
    )
    .map_err(ContextCommandError::Prepare)
}

fn write_human_context(
    writer: &mut dyn Write,
    name: &str,
    context: &ExecutionContext,
) -> Result<(), io::Error> {
    writeln!(writer, "context: {}", sanitize_terminal_text(name))?;
    if let Some(shell) = &context.shell {
        writeln!(writer, "shell: {}", sanitize_terminal_text(shell))?;
    }
    writeln!(writer, "confirm: {}", context.confirm)?;
    if context.secrets.is_empty() {
        writeln!(writer, "secrets: (none)")?;
    } else {
        writeln!(writer, "secrets:")?;
        for (slot, secret) in &context.secrets {
            writeln!(
                writer,
                "  {}: ref={} delivery={} provider={}",
                sanitize_terminal_text(slot),
                sanitize_terminal_text(&secret.reference),
                delivery_label(secret_delivery_mode(secret)),
                provider_label(secret_provider_mode(secret)),
            )?;
        }
    }
    Ok(())
}

fn delivery_label(delivery: SecretDelivery) -> &'static str {
    match delivery {
        SecretDelivery::Env => "env",
        SecretDelivery::File => "file",
        SecretDelivery::Stdin => "stdin",
    }
}

fn provider_label(provider: SecretProvider) -> &'static str {
    match provider {
        SecretProvider::Env => "env",
        SecretProvider::File => "file",
        SecretProvider::Sops => "sops",
        SecretProvider::SopsNix => "sops-nix",
    }
}

#[derive(Serialize)]
struct ContextInspectView {
    name: String,
    shell: Option<String>,
    confirm: bool,
    secrets: BTreeMap<String, ContextSecretInspectView>,
}

#[derive(Serialize)]
struct ContextSecretInspectView {
    #[serde(rename = "ref")]
    reference: String,
    delivery: String,
    provider: String,
}

impl ContextInspectView {
    fn from_context(name: &str, context: &ExecutionContext) -> Self {
        let secrets = context
            .secrets
            .iter()
            .map(|(slot, secret)| {
                (
                    slot.clone(),
                    ContextSecretInspectView {
                        reference: secret.reference.clone(),
                        delivery: delivery_label(secret_delivery_mode(secret)).to_owned(),
                        provider: provider_label(secret_provider_mode(secret)).to_owned(),
                    },
                )
            })
            .collect();
        Self {
            name: name.to_owned(),
            shell: context.shell.clone(),
            confirm: context.confirm,
            secrets,
        }
    }
}
