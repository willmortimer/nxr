//! `nxr plan` command implementation.
//!
//! Resolves `name` as an app first (V1 behavior). When no app matches, treats
//! `name` as a task (including aliases) and emits an [`ExecutionPlan`] envelope.

use std::io::{self, Write};

use nxr_core::Plan;
use nxr_core::diagnostics::exit;
use nxr_nix::TaskDiscoveryError;
use nxr_task::{
    ExecutionPlan, FailurePolicy, PlanError as TaskPlanError, ResolveTaskError,
    build_execution_plan, resolve_task_name,
};

use crate::commands::common::{
    AppRequest, PrepareError, build_adapter, current_invocation_directory, prepare_app_plan,
};
use crate::commands::task::plan_exit_code;
use crate::flake::resolve_flake;
use crate::runner_output::RunnerOutput;

/// Errors while printing a plan.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Discovery(#[from] TaskDiscoveryError),
    #[error(transparent)]
    TaskNotFound(#[from] ResolveTaskError),
    #[error(transparent)]
    TaskPlan(#[from] TaskPlanError),
    #[error(transparent)]
    Render(#[from] PlanRenderError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl PlanError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Discovery(error) => error.exit_code(),
            Self::TaskNotFound(_) => exit::NOT_FOUND,
            Self::TaskPlan(error) => plan_exit_code(error),
            Self::Render(_) | Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Errors while rendering plan output.
#[derive(Debug, thiserror::Error)]
pub enum PlanRenderError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolve an app or task and print its execution plan.
///
/// Apps win when both an app and a task share a name. Task aliases are resolved
/// only after app lookup fails. Bare `nxr <name>` does not use this path.
///
/// # Errors
///
/// Returns [`PlanError`] when planning or rendering fails.
pub fn run(request: &AppRequest<'_>, json: bool, runner: RunnerOutput) -> Result<(), PlanError> {
    match prepare_app_plan(request) {
        Ok(prepared) => {
            runner
                .info(format!("planning app {}", prepared.plan.target))
                .map_err(PlanError::Io)?;
            let mut stdout = io::stdout().lock();
            write_plan(&mut stdout, &prepared.plan, json)?;
            Ok(())
        }
        Err(PrepareError::NotFound(_)) => plan_task(request, json, runner),
        Err(error) => Err(PlanError::Prepare(error)),
    }
}

fn plan_task(request: &AppRequest<'_>, json: bool, runner: RunnerOutput) -> Result<(), PlanError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)
        .map_err(|error| PlanError::Prepare(PrepareError::Flake(error)))?;
    let adapter = build_adapter(request.nix_override)
        .map_err(|error| PlanError::Prepare(PrepareError::Nix(error)))?;
    let document = adapter.discover_tasks(&flake.nix_ref)?;
    let canonical = resolve_task_name(&document, request.app)?;
    let plan = build_execution_plan(&document.tasks, canonical, FailurePolicy::FailFast, None)?;

    runner
        .info(format!("planning task {canonical}"))
        .map_err(PlanError::Io)?;
    let mut stdout = io::stdout().lock();
    write_execution_plan(&mut stdout, &plan, json)?;
    Ok(())
}

/// Write an app [`Plan`] as JSON or a concise human summary.
///
/// # Errors
///
/// Returns [`PlanRenderError`] when serialization or writing fails.
pub fn write_plan(writer: &mut impl Write, plan: &Plan, json: bool) -> Result<(), PlanRenderError> {
    if json {
        let rendered = serde_json::to_string_pretty(plan)?;
        writeln!(writer, "{rendered}")?;
    } else {
        write_human_plan(writer, plan)?;
    }
    Ok(())
}

/// Write a task [`ExecutionPlan`] as JSON or a concise human summary.
///
/// # Errors
///
/// Returns [`PlanRenderError`] when serialization or writing fails.
pub fn write_execution_plan(
    writer: &mut impl Write,
    plan: &ExecutionPlan,
    json: bool,
) -> Result<(), PlanRenderError> {
    if json {
        let rendered = serde_json::to_string_pretty(plan)?;
        writeln!(writer, "{rendered}")?;
    } else {
        write_human_execution_plan(writer, plan)?;
    }
    Ok(())
}

fn write_human_plan(writer: &mut impl Write, plan: &Plan) -> io::Result<()> {
    write!(writer, "{}", plan.command.program)?;
    for argument in &plan.command.arguments {
        write!(writer, " {argument}")?;
    }
    writeln!(writer)?;
    if let Some(shell) = &plan.shell {
        writeln!(writer, "shell: {shell}")?;
    }
    writeln!(writer, "execution_directory: {}", plan.execution_directory)?;
    Ok(())
}

fn write_human_execution_plan(writer: &mut impl Write, plan: &ExecutionPlan) -> io::Result<()> {
    writeln!(writer, "root: {}", plan.root)?;
    writeln!(writer, "failure_policy: {}", plan.failure_policy.as_str())?;
    writeln!(writer, "serial_order: {}", plan.serial_order.join(" -> "))?;
    writeln!(writer, "nodes: {}", plan.nodes.len())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind};
    use nxr_task::{FailurePolicy, build_serial_plan};

    use super::{write_execution_plan, write_plan};

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
            shell: None,
            environment_policy: EnvironmentPolicy::Inherit,
            command: PlanCommand {
                program: "/nix/bin/nix".to_owned(),
                arguments: vec!["run".to_owned(), "/project#hello".to_owned()],
            },
            forwarded_arguments: vec![],
        }
    }

    #[test]
    fn human_plan_prints_command_and_execution_directory() {
        let mut output = Vec::new();
        write_plan(&mut output, &sample_plan(), false).expect("write plan");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("/nix/bin/nix run /project#hello\n"));
        assert!(rendered.contains("execution_directory: /project\n"));
    }

    #[test]
    fn json_plan_includes_schema_version_and_command() {
        let mut output = Vec::new();
        write_plan(&mut output, &sample_plan(), true).expect("write plan");
        let value: serde_json::Value = serde_json::from_slice(&output).expect("parse plan json");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["command"]["program"], "/nix/bin/nix");
        assert_eq!(value["target"], "hello");
    }

    #[test]
    fn human_execution_plan_prints_serial_order() {
        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert("a".to_owned(), nxr_task::TaskDefinition::new("a"));
        let mut b = nxr_task::TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        tasks.insert("b".to_owned(), b);
        let plan = build_serial_plan(&tasks, "b").expect("plan");

        let mut output = Vec::new();
        write_execution_plan(&mut output, &plan, false).expect("write execution plan");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("root: b\n"));
        assert!(rendered.contains("serial_order: a -> b\n"));
        assert_eq!(plan.failure_policy, FailurePolicy::FailFast);
    }
}
