//! `nxr explain` — resolution and invocation diagnostics for apps and tasks.

use std::collections::BTreeMap;
use std::io::{self, Write};

use nxr_completion::cache::{DiscoveryCacheEntry, DiscoveryContext, discovery_cache_entry};
use nxr_core::diagnostics::exit;
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_core::{EnvironmentPolicy, Plan, PlanCommand};
use nxr_nix::{NixAdapter, NixCapabilities, OptionalNixFlags};
use nxr_task::{ExecutionPlan, FailurePolicy, PlanError, build_execution_plan, resolve_task_name};
use serde::Serialize;

use crate::commands::common::{AppRequest, PrepareError, PreparedTaskNode, WorkspaceSnapshot};
use crate::commands::task::task_inherits_stdin;
use crate::flake::FlakeSelection;
use crate::output_task::{EventsFormat, TaskOutputMode};
use crate::runner_output::RunnerOutput;
use crate::shell_mode::{ShellMode, active_dev_shell, should_wrap_shell_with_active};

const SCHEMA_VERSION: u32 = 1;

/// Explicit target kind for `nxr explain app` / `nxr explain task`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplainKind {
    App,
    Task,
}

/// Inputs for `nxr explain`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplainRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub name: &'a str,
    pub kind: Option<ExplainKind>,
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_mode: ShellMode,
    pub environment_policy: EnvironmentPolicy,
    pub jobs: usize,
    pub output_mode: Option<TaskOutputMode>,
    pub events_format: Option<EventsFormat>,
    pub nix_flags: &'a OptionalNixFlags,
}

/// Errors while running explain.
#[derive(Debug, thiserror::Error)]
pub enum ExplainError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl ExplainError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Plan(error) => match error {
                PlanError::UnknownRoot { .. } => exit::NOT_FOUND,
                PlanError::MissingDependency { .. } | PlanError::Cycle { .. } => exit::TASK_GRAPH,
            },
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

/// Shared workspace context for explain and doctor `--all`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceContext {
    pub flake: FlakeContext,
    pub system: String,
    pub nix: NixContext,
    pub discovery_cache: DiscoveryCacheEntry,
    pub invocation_directory: String,
    pub requested_shell: Option<String>,
    pub active_shell: Option<String>,
    pub environment_policy: EnvironmentPolicy,
}

/// Flake selection summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FlakeContext {
    pub display: String,
    pub nix_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_root: Option<String>,
}

/// Nix executable and negotiated capabilities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NixContext {
    pub executable: String,
    pub version: String,
    pub capabilities: NixCapabilities,
}

/// Versioned explain envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExplainReport {
    App {
        schema_version: u32,
        workspace: WorkspaceContext,
        target: String,
        attr_path: String,
        execution_directory: String,
        shell_wrap: ShellWrapContext,
        command: PlanCommand,
        forwarded_arguments: Vec<String>,
    },
    Task {
        schema_version: u32,
        workspace: WorkspaceContext,
        target: String,
        failure_policy: FailurePolicy,
        argument_forwarding: String,
        stdin_policy: String,
        dependency_path: Vec<String>,
        shell_wrap: ShellWrapContext,
        nodes: Vec<TaskNodeExplain>,
    },
}

/// Whether `nix develop` wrapping is applied and why it may be skipped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShellWrapContext {
    pub requested_shell: Option<String>,
    pub active_shell: Option<String>,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

/// One prepared task node in an explain report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TaskNodeExplain {
    pub id: String,
    pub app: String,
    pub attr_path: String,
    pub execution_directory: String,
    pub forwarded_arguments: Vec<String>,
    pub command: PlanCommand,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skip_reasons: Vec<String>,
}

/// Build workspace diagnostics from a loaded snapshot.
///
/// # Errors
///
/// Returns [`io::Error`] when discovery cache metadata cannot be read.
pub fn workspace_context_from_snapshot(
    snapshot: &WorkspaceSnapshot,
    environment_policy: &EnvironmentPolicy,
    shell: Option<&str>,
) -> io::Result<WorkspaceContext> {
    let discovery_cache = discovery_cache_entry(&DiscoveryContext {
        flake_ref: snapshot.flake.nix_ref.clone(),
        local_root: snapshot.flake.local_root.clone(),
        system: snapshot.nix.system.clone(),
    })?;

    Ok(WorkspaceContext {
        flake: flake_context(&snapshot.flake),
        system: snapshot.nix.system.clone(),
        nix: nix_context(&snapshot.nix),
        discovery_cache,
        invocation_directory: snapshot.invocation_directory.as_str().to_owned(),
        requested_shell: shell.map(str::to_owned),
        active_shell: active_dev_shell(),
        environment_policy: environment_policy.clone(),
    })
}

/// Resolve and print explain output for an app or task target.
///
/// # Errors
///
/// Returns [`ExplainError`] when resolution, planning, or rendering fails.
pub fn run(
    request: ExplainRequest<'_>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), ExplainError> {
    let report = match request.kind {
        Some(ExplainKind::Task) => explain_task(&request)?,
        Some(ExplainKind::App) => explain_app(&request)?,
        None => match explain_app(&request) {
            Ok(report) => report,
            Err(ExplainError::Prepare(PrepareError::NotFound(_))) => explain_task(&request)?,
            Err(error) => return Err(error),
        },
    };

    runner
        .info(format!("explaining {}", request.name))
        .map_err(ExplainError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        let rendered = serde_json::to_string_pretty(&report)?;
        writeln!(stdout, "{rendered}")?;
    } else {
        write_human_report(&mut stdout, &report)?;
    }
    Ok(())
}

fn explain_app(request: &ExplainRequest<'_>) -> Result<ExplainReport, ExplainError> {
    let app_request = app_request_from_explain(request);
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        false,
        request.nix_flags,
    )?;
    let prepared = snapshot.prepare_discovered_app(&app_request)?;
    let workspace =
        workspace_context_from_snapshot(&snapshot, &request.environment_policy, request.shell)?;
    let shell_wrap = shell_wrap_context(
        request.shell,
        request.shell_mode,
        workspace.active_shell.as_deref(),
        &prepared.plan,
    );

    Ok(ExplainReport::App {
        schema_version: SCHEMA_VERSION,
        workspace,
        target: prepared.plan.target.clone(),
        attr_path: prepared.plan.attr_path.clone(),
        execution_directory: prepared.plan.execution_directory.clone(),
        shell_wrap,
        command: prepared.plan.command.clone(),
        forwarded_arguments: prepared.plan.forwarded_arguments.clone(),
    })
}

fn explain_task(request: &ExplainRequest<'_>) -> Result<ExplainReport, ExplainError> {
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        true,
        request.nix_flags,
    )?;
    let document = snapshot
        .tasks
        .as_ref()
        .expect("load_tasks=true always populates tasks")
        .clone();
    let canonical = resolve_task_name(&document, request.name)
        .map_err(|error| ExplainError::Plan(PlanError::UnknownRoot { root: error.name }))?;
    let plan = build_execution_plan(&document.tasks, canonical, FailurePolicy::FailFast, None)?;
    snapshot
        .validate_task_apps(&document)
        .map_err(PrepareError::NotFound)?;
    let root_task_ids: &[String] = if plan.roots.is_empty() {
        std::slice::from_ref(&plan.root)
    } else {
        &plan.roots
    };
    let prepared_nodes = snapshot.prepare_task_nodes(
        &document,
        root_task_ids,
        &plan.serial_order,
        request.args,
        request.root,
        request.cwd,
        request.shell,
        request.shell_mode,
        &request.environment_policy,
        request.nix_flags,
    )?;

    let workspace =
        workspace_context_from_snapshot(&snapshot, &request.environment_policy, request.shell)?;
    let shell_wrap = shell_wrap_context(
        request.shell,
        request.shell_mode,
        workspace.active_shell.as_deref(),
        prepared_nodes
            .get(&plan.root)
            .map(|node| &node.plan)
            .expect("root node prepared"),
    );
    let stdin_policy =
        if task_inherits_stdin(request.jobs, request.output_mode, request.events_format) {
            "inherit"
        } else {
            "null"
        };

    let nodes = task_nodes_explain(&plan, &document, &prepared_nodes);

    Ok(ExplainReport::Task {
        schema_version: SCHEMA_VERSION,
        workspace,
        target: plan.root.clone(),
        failure_policy: plan.failure_policy,
        argument_forwarding: plan.argument_forwarding.as_str().to_owned(),
        stdin_policy: stdin_policy.to_owned(),
        dependency_path: plan.serial_order.clone(),
        shell_wrap,
        nodes,
    })
}

fn task_nodes_explain(
    plan: &ExecutionPlan,
    document: &nxr_task::TaskDocument,
    prepared_nodes: &BTreeMap<String, PreparedTaskNode>,
) -> Vec<TaskNodeExplain> {
    plan.serial_order
        .iter()
        .map(|task_id| {
            let node = prepared_nodes
                .get(task_id)
                .expect("serial_order only contains prepared nodes");
            let definition = document
                .tasks
                .get(task_id)
                .expect("serial_order only contains known tasks");
            TaskNodeExplain {
                id: task_id.clone(),
                app: definition.app.clone(),
                attr_path: node.plan.attr_path.clone(),
                execution_directory: node.plan.execution_directory.clone(),
                forwarded_arguments: node.plan.forwarded_arguments.clone(),
                command: node.plan.command.clone(),
                skip_reasons: scheduler_skip_reasons(task_id, plan),
            }
        })
        .collect()
}

fn scheduler_skip_reasons(task_id: &str, plan: &ExecutionPlan) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(node) = plan.nodes.iter().find(|node| node.id == task_id) {
        if !node.depends_on.is_empty() {
            match plan.failure_policy {
                FailurePolicy::FailFast => reasons.push(
                    "not started after an upstream failure under fail-fast policy".to_owned(),
                ),
                FailurePolicy::KeepGoing => reasons.push(
                    "cancelled when a direct dependency fails under keep-going policy".to_owned(),
                ),
            }
        }
    }
    reasons
}

fn app_request_from_explain<'a>(request: &'a ExplainRequest<'a>) -> AppRequest<'a> {
    AppRequest {
        flake_arg: request.flake_arg,
        nix_override: request.nix_override,
        app: request.name,
        args: request.args,
        root: request.root,
        cwd: request.cwd,
        shell: request.shell,
        shell_mode: request.shell_mode,
        environment_policy: request.environment_policy.clone(),
        nix_flags: request.nix_flags,
    }
}

fn flake_context(flake: &FlakeSelection) -> FlakeContext {
    FlakeContext {
        display: flake.display.clone(),
        nix_ref: flake.nix_ref.clone(),
        local_root: flake
            .local_root
            .as_ref()
            .map(|path| path.as_str().to_owned()),
    }
}

fn nix_context(adapter: &NixAdapter) -> NixContext {
    NixContext {
        executable: adapter.nix.as_str().to_owned(),
        version: adapter.capabilities.version.to_string(),
        capabilities: adapter.capabilities.clone(),
    }
}

fn shell_wrap_context(
    requested: Option<&str>,
    mode: ShellMode,
    active: Option<&str>,
    _plan: &Plan,
) -> ShellWrapContext {
    let applied = requested.is_some_and(|name| should_wrap_shell_with_active(name, mode, active));
    let skip_reason = requested.and_then(|name| {
        if applied {
            return None;
        }
        match mode {
            ShellMode::Never => Some("shell-mode is never".to_owned()),
            ShellMode::Smart if active == Some(name) => {
                Some(format!("active dev shell matches requested shell ({name})"))
            }
            ShellMode::Smart => Some("shell wrap not applied".to_owned()),
            ShellMode::Always => Some("shell wrap not applied".to_owned()),
        }
    });

    ShellWrapContext {
        requested_shell: requested.map(str::to_owned),
        active_shell: active.map(str::to_owned),
        applied,
        skip_reason,
    }
}

fn write_human_report(writer: &mut impl Write, report: &ExplainReport) -> io::Result<()> {
    match report {
        ExplainReport::App {
            workspace,
            target,
            attr_path,
            execution_directory,
            shell_wrap,
            command,
            forwarded_arguments,
            ..
        } => {
            write_workspace_header(writer, workspace)?;
            writeln!(writer, "kind: app")?;
            writeln!(writer, "target: {target}")?;
            writeln!(writer, "attr_path: {attr_path}")?;
            writeln!(writer, "execution_directory: {execution_directory}")?;
            write_shell_wrap(writer, shell_wrap)?;
            write_command(writer, command)?;
            if !forwarded_arguments.is_empty() {
                writeln!(
                    writer,
                    "forwarded_arguments: {}",
                    forwarded_arguments.join(" ")
                )?;
            }
        }
        ExplainReport::Task {
            workspace,
            target,
            failure_policy,
            argument_forwarding,
            stdin_policy,
            dependency_path,
            shell_wrap,
            nodes,
            ..
        } => {
            write_workspace_header(writer, workspace)?;
            writeln!(writer, "kind: task")?;
            writeln!(writer, "target: {target}")?;
            writeln!(writer, "failure_policy: {}", failure_policy.as_str())?;
            writeln!(writer, "argument_forwarding: {argument_forwarding}")?;
            writeln!(writer, "stdin_policy: {stdin_policy}")?;
            writeln!(writer, "dependency_path: {}", dependency_path.join(" -> "))?;
            write_shell_wrap(writer, shell_wrap)?;
            for node in nodes {
                writeln!(writer)?;
                writeln!(writer, "[{}]", node.id)?;
                writeln!(writer, "app: {}", node.app)?;
                writeln!(writer, "attr_path: {}", node.attr_path)?;
                writeln!(writer, "execution_directory: {}", node.execution_directory)?;
                write_command(writer, &node.command)?;
                if !node.forwarded_arguments.is_empty() {
                    writeln!(
                        writer,
                        "forwarded_arguments: {}",
                        node.forwarded_arguments.join(" ")
                    )?;
                }
                for reason in &node.skip_reasons {
                    writeln!(writer, "skip_reason: {}", sanitize_terminal_text(reason))?;
                }
            }
        }
    }
    Ok(())
}

fn write_workspace_header(writer: &mut impl Write, workspace: &WorkspaceContext) -> io::Result<()> {
    writeln!(writer, "flake: {}", workspace.flake.display)?;
    if let Some(root) = &workspace.flake.local_root {
        writeln!(writer, "flake_root: {root}")?;
    }
    writeln!(writer, "system: {}", workspace.system)?;
    writeln!(
        writer,
        "nix: {} ({})",
        workspace.nix.executable, workspace.nix.version
    )?;
    writeln!(
        writer,
        "discovery_cache: hit={} invalidation_key={}",
        workspace.discovery_cache.hit,
        workspace
            .discovery_cache
            .invalidation_key
            .map(|key| key.to_string())
            .unwrap_or_else(|| "n/a".to_owned())
    )?;
    if let Some(file) = &workspace.discovery_cache.cache_file {
        writeln!(writer, "cache_file: {file}")?;
    }
    writeln!(
        writer,
        "invocation_directory: {}",
        workspace.invocation_directory
    )?;
    writeln!(
        writer,
        "environment_policy: {}",
        environment_policy_label(&workspace.environment_policy)
    )?;
    if let Some(shell) = &workspace.requested_shell {
        writeln!(writer, "requested_shell: {shell}")?;
    }
    if let Some(shell) = &workspace.active_shell {
        writeln!(writer, "active_shell: {shell}")?;
    }
    Ok(())
}

fn write_shell_wrap(writer: &mut impl Write, shell_wrap: &ShellWrapContext) -> io::Result<()> {
    writeln!(
        writer,
        "shell_wrap: {}",
        if shell_wrap.applied {
            "applied"
        } else {
            "skipped"
        }
    )?;
    if let Some(reason) = &shell_wrap.skip_reason {
        writeln!(
            writer,
            "shell_wrap_reason: {}",
            sanitize_terminal_text(reason)
        )?;
    }
    Ok(())
}

fn write_command(writer: &mut impl Write, command: &PlanCommand) -> io::Result<()> {
    write!(writer, "command: {}", command.program)?;
    for argument in &command.arguments {
        write!(writer, " {argument}")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn environment_policy_label(policy: &EnvironmentPolicy) -> &'static str {
    match policy {
        EnvironmentPolicy::Inherit => "inherit",
        EnvironmentPolicy::Clean { .. } => "clean",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind};
    use nxr_task::{FailurePolicy, TaskDefinition, build_serial_plan};

    use super::{
        ExplainReport, ShellWrapContext, TaskNodeExplain, scheduler_skip_reasons,
        shell_wrap_context, write_human_report,
    };
    use crate::shell_mode::ShellMode;

    fn sample_plan() -> Plan {
        Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: "/project".to_owned(),
            system: "aarch64-darwin".to_owned(),
            target: "hello".to_owned(),
            attr_path: "apps.aarch64-darwin.hello".to_owned(),
            invocation_directory: "/project".to_owned(),
            execution_directory: "/project".to_owned(),
            shell: Some("default".to_owned()),
            active_shell: Some("default".to_owned()),
            environment_policy: EnvironmentPolicy::Inherit,
            command: PlanCommand {
                program: "/nix/bin/nix".to_owned(),
                arguments: vec![
                    "run".to_owned(),
                    "/project#hello".to_owned(),
                    "--".to_owned(),
                    "one".to_owned(),
                ],
            },
            forwarded_arguments: vec!["one".to_owned()],
        }
    }

    #[test]
    fn shell_wrap_smart_skips_when_active_matches() {
        let wrap = shell_wrap_context(
            Some("default"),
            ShellMode::Smart,
            Some("default"),
            &sample_plan(),
        );
        assert!(!wrap.applied);
        assert_eq!(
            wrap.skip_reason.as_deref(),
            Some("active dev shell matches requested shell (default)")
        );
    }

    #[test]
    fn scheduler_skip_reasons_document_failure_policies() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("a"));
        let mut b = TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        tasks.insert("b".to_owned(), b);
        let plan = build_serial_plan(&tasks, "b").expect("plan");
        let reasons = scheduler_skip_reasons("b", &plan);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("fail-fast"));
    }

    #[test]
    fn human_task_report_includes_dependency_path_and_nodes() {
        let report = ExplainReport::Task {
            schema_version: 1,
            workspace: super::WorkspaceContext {
                flake: super::FlakeContext {
                    display: "fixtures/task-dag".to_owned(),
                    nix_ref: "/abs/fixtures/task-dag".to_owned(),
                    local_root: Some("/abs/fixtures/task-dag".to_owned()),
                },
                system: "aarch64-darwin".to_owned(),
                nix: super::NixContext {
                    executable: "/nix/bin/nix".to_owned(),
                    version: "2.18.1".to_owned(),
                    capabilities: nxr_nix::NixCapabilities::all_supported_for_tests(
                        nxr_nix::NixVersion::new(2, 18, 1),
                    ),
                },
                discovery_cache: nxr_completion::DiscoveryCacheEntry {
                    available: true,
                    directory: "/tmp/nxr-cache".to_owned(),
                    cache_file: Some("/tmp/nxr-cache/abc.json".to_owned()),
                    hit: true,
                    invalidation_key: Some(42),
                    cached_invalidation_key: Some(42),
                },
                invocation_directory: "/work".to_owned(),
                requested_shell: None,
                active_shell: None,
                environment_policy: EnvironmentPolicy::Inherit,
            },
            target: "ci".to_owned(),
            failure_policy: FailurePolicy::FailFast,
            argument_forwarding: "root".to_owned(),
            stdin_policy: "inherit".to_owned(),
            dependency_path: vec!["fmt".to_owned(), "test".to_owned(), "ci".to_owned()],
            shell_wrap: ShellWrapContext {
                requested_shell: None,
                active_shell: None,
                applied: false,
                skip_reason: None,
            },
            nodes: vec![TaskNodeExplain {
                id: "fmt".to_owned(),
                app: "fmt".to_owned(),
                attr_path: "apps.aarch64-darwin.fmt".to_owned(),
                execution_directory: "/work".to_owned(),
                forwarded_arguments: Vec::new(),
                command: PlanCommand {
                    program: "/nix/bin/nix".to_owned(),
                    arguments: vec!["run".to_owned(), "/abs/fixtures/task-dag#fmt".to_owned()],
                },
                skip_reasons: Vec::new(),
            }],
        };

        let mut output = Vec::new();
        write_human_report(&mut output, &report).expect("write human explain");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("kind: task\n"));
        assert!(rendered.contains("dependency_path: fmt -> test -> ci\n"));
        assert!(rendered.contains("[fmt]\n"));
        assert!(rendered.contains("invalidation_key=42\n"));
    }

    #[test]
    fn golden_app_explain_json_shape() {
        let report = ExplainReport::App {
            schema_version: 1,
            workspace: super::WorkspaceContext {
                flake: super::FlakeContext {
                    display: "fixtures/basic-apps".to_owned(),
                    nix_ref: "/abs/fixtures/basic-apps".to_owned(),
                    local_root: Some("/abs/fixtures/basic-apps".to_owned()),
                },
                system: "aarch64-darwin".to_owned(),
                nix: super::NixContext {
                    executable: "/nix/bin/nix".to_owned(),
                    version: "2.18.1".to_owned(),
                    capabilities: nxr_nix::NixCapabilities::all_supported_for_tests(
                        nxr_nix::NixVersion::new(2, 18, 1),
                    ),
                },
                discovery_cache: nxr_completion::DiscoveryCacheEntry {
                    available: true,
                    directory: "/tmp/nxr-cache".to_owned(),
                    cache_file: Some("/tmp/nxr-cache/deadbeef.json".to_owned()),
                    hit: false,
                    invalidation_key: Some(99),
                    cached_invalidation_key: Some(88),
                },
                invocation_directory: "/work".to_owned(),
                requested_shell: None,
                active_shell: None,
                environment_policy: EnvironmentPolicy::Inherit,
            },
            target: "hello".to_owned(),
            attr_path: "apps.aarch64-darwin.hello".to_owned(),
            execution_directory: "/work".to_owned(),
            shell_wrap: ShellWrapContext {
                requested_shell: None,
                active_shell: None,
                applied: false,
                skip_reason: None,
            },
            command: PlanCommand {
                program: "/nix/bin/nix".to_owned(),
                arguments: vec![
                    "run".to_owned(),
                    "/abs/fixtures/basic-apps#hello".to_owned(),
                ],
            },
            forwarded_arguments: Vec::new(),
        };

        let rendered = serde_json::to_string_pretty(&report).expect("serialize explain json");
        let expected = include_str!("../../../../tests/fixtures/explain-basic-apps-hello.json");
        assert_eq!(rendered.trim_end(), expected.trim_end());
    }
}
