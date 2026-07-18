//! `nxr plan` command implementation.

use std::io::{self, Write};

use nxr_core::Plan;
use nxr_core::diagnostics::exit;

use crate::commands::common::{AppRequest, PrepareError, prepare_app_plan};

/// Errors while printing a plan.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Render(#[from] PlanRenderError),
}

impl PlanError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Render(_) => exit::EVALUATION,
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

/// Resolve an app and print its execution plan.
///
/// # Errors
///
/// Returns [`PlanError`] when planning or rendering fails.
pub fn run(request: AppRequest<'_>, json: bool) -> Result<(), PlanError> {
    let prepared = prepare_app_plan(request)?;
    let mut stdout = io::stdout().lock();
    write_plan(&mut stdout, &prepared.plan, json)?;
    Ok(())
}

/// Write a plan as JSON or a concise human summary.
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

fn write_human_plan(writer: &mut impl Write, plan: &Plan) -> io::Result<()> {
    write!(writer, "{}", plan.command.program)?;
    for argument in &plan.command.arguments {
        write!(writer, " {argument}")?;
    }
    writeln!(writer)?;
    writeln!(writer, "execution_directory: {}", plan.execution_directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind};

    use super::write_plan;

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
}
