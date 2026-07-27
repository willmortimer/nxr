//! `nxr ci plan` — provider-neutral CI plan export.

use std::io::{self, Write};

use nxr_affected::AffectedAnalysis;
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_task::{
    ExecutionPlan, FailurePolicy, PlanError as TaskPlanError, TaskDocument,
    build_execution_plan_roots,
};
use serde::Serialize;

use crate::commands::affected::{AffectedCommandError, AffectedPathSources, select_for_flake};
use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::commands::task::plan_exit_code;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Supported major version for CI plan JSON.
pub const CI_PLAN_SCHEMA_VERSION: u32 = 1;

/// Errors while building a CI plan.
#[derive(Debug, thiserror::Error)]
pub enum CiPlanError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Affected(#[from] AffectedCommandError),
    #[error(transparent)]
    TaskPlan(#[from] TaskPlanError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Usage(String),
}

impl CiPlanError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Affected(error) => error.exit_code(),
            Self::TaskPlan(error) => plan_exit_code(error),
            Self::Io(_) | Self::Json(_) => nxr_core::diagnostics::exit::EVALUATION,
            Self::Usage(_) => nxr_core::diagnostics::exit::USAGE,
        }
    }
}

/// Versioned CI plan envelope (`ci-plan-v1`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CiPlanEnvelope {
    pub schema_version: u32,
    pub flake: String,
    pub system: String,
    pub roots: Vec<String>,
    pub execution_plan: ExecutionPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected: Option<AffectedAnalysis>,
}

/// Inputs for `nxr ci plan`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiPlanRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub refresh_discovery: bool,
    pub json: bool,
    pub nix_flags: &'a OptionalNixFlags,
    pub path_sources: &'a AffectedPathSources,
    pub strict: bool,
    pub paths: &'a [String],
    pub roots: &'a [String],
}

/// Build and print a CI plan for the selected flake.
///
/// When path sources are provided, roots are intersected with the affected task
/// set (same policy as `nxr task --affected`). Without path sources, roots default
/// to an explicit `ci` task when present, otherwise every sink task in the DAG.
///
/// # Errors
///
/// Returns [`CiPlanError`] when discovery, affected analysis, or planning fails.
#[allow(clippy::too_many_arguments)]
pub fn plan_run(request: &CiPlanRequest<'_>, runner: RunnerOutput) -> Result<(), CiPlanError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;
    let document = discover_tasks(
        &flake,
        &adapter,
        request.refresh_discovery,
        request.nix_flags,
    )?;

    let has_path_source = !request.paths.is_empty() || request.path_sources.needs_git();
    let (roots, affected) = if has_path_source {
        let selection = select_for_flake(
            request.flake_arg,
            request.nix_override,
            request.refresh_discovery,
            request.nix_flags,
            request.path_sources,
            request.strict,
            request.paths,
            runner.clone(),
        )?;
        let requested = if request.roots.is_empty() {
            Vec::new()
        } else {
            request.roots.to_vec()
        };
        let roots = crate::commands::affected::resolve_affected_task_roots(
            &selection.document,
            &selection.analysis,
            &requested,
        )?;
        (roots, Some(selection.analysis))
    } else {
        let roots = if request.roots.is_empty() {
            default_ci_roots(&document)
        } else {
            request.roots.to_vec()
        };
        (roots, None)
    };

    if roots.is_empty() {
        return Err(CiPlanError::Usage(
            "no CI task roots to plan; add tasks, pass explicit roots, or provide a path source for affected selection"
                .to_owned(),
        ));
    }

    let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    let execution_plan =
        build_execution_plan_roots(&document.tasks, &root_refs, FailurePolicy::FailFast, None)?;

    runner
        .info(format!(
            "CI plan for {} ({}): {}",
            flake.display,
            adapter.system,
            roots.join(", ")
        ))
        .map_err(CiPlanError::Io)?;

    let envelope = CiPlanEnvelope {
        schema_version: CI_PLAN_SCHEMA_VERSION,
        flake: flake.display,
        system: adapter.system,
        roots,
        execution_plan,
        affected,
    };

    if request.json {
        let rendered = serde_json::to_string_pretty(&envelope)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        write_human(&mut io::stdout().lock(), &envelope)?;
    }

    Ok(())
}

fn default_ci_roots(document: &TaskDocument) -> Vec<String> {
    if document.tasks.contains_key("ci") {
        return vec!["ci".to_owned()];
    }

    let dependents: std::collections::BTreeSet<String> = document
        .tasks
        .values()
        .flat_map(|task| task.depends_on.iter().cloned())
        .collect();
    document
        .tasks
        .keys()
        .filter(|name| !dependents.contains(*name))
        .cloned()
        .collect()
}

fn discover_tasks(
    flake: &crate::flake::FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, CiPlanError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };
    let flake_ref = flake.nix_ref.clone();
    let workspace = discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions::with_tasks(refresh_discovery),
        || -> Result<WorkspaceDiscovery, CiPlanError> {
            let apps = adapter
                .discover_apps(&flake_ref, nix_flags)
                .map_err(CiPlanError::Nix)?;
            let tasks = adapter
                .discover_tasks(&flake_ref, nix_flags)
                .map_err(CiPlanError::Tasks)?;
            Ok(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
                ..Default::default()
            })
        },
    )?;
    workspace
        .tasks
        .ok_or_else(|| CiPlanError::Usage("flake has no nxr tasks".to_owned()))
}

fn write_human(writer: &mut impl Write, envelope: &CiPlanEnvelope) -> io::Result<()> {
    writeln!(
        writer,
        "CI plan for {} ({})",
        envelope.flake, envelope.system
    )?;
    writeln!(writer, "roots: {}", envelope.roots.join(", "))?;
    writeln!(
        writer,
        "serial_order: {}",
        envelope.execution_plan.serial_order.join(" -> ")
    )?;
    if let Some(affected) = &envelope.affected {
        writeln!(
            writer,
            "affected: {} changed path(s), {} task(s)",
            affected.changed_paths.len(),
            affected.tasks.len()
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_task::{TaskDefinition, TaskDocument};

    use super::default_ci_roots;

    #[test]
    fn default_roots_prefer_ci_task() {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), TaskDefinition::new("fmt"));
        tasks.insert("ci".to_owned(), TaskDefinition::new("ci"));
        let doc = TaskDocument::new(tasks);
        assert_eq!(default_ci_roots(&doc), vec!["ci".to_owned()]);
    }

    #[test]
    fn default_roots_use_sink_tasks_without_ci() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), TaskDefinition::new("a"));
        let mut b = TaskDefinition::new("b");
        b.depends_on = vec!["a".to_owned()];
        tasks.insert("b".to_owned(), b);
        let doc = TaskDocument::new(tasks);
        assert_eq!(default_ci_roots(&doc), vec!["b".to_owned()]);
    }
}
