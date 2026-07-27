//! `nxr cache` subcommands.

use std::io::{self, Write};

use nxr_completion::{clear_discovery_cache, discovery_cache_status};
use nxr_core::cas::CacheExplain;
use nxr_core::diagnostics::exit;
use nxr_nix::{capability_cache_status, clear_capability_cache};
use nxr_task::{PlanError, build_execution_plan_roots, resolve_task_name};
use serde::Serialize;

use crate::commands::common::{PrepareError, WorkspaceSnapshot};
use crate::commands::workspace_cache::explain_workspace_cache;
use crate::flake::FlakeResolveError;
use crate::runner_output::RunnerOutput;
use nxr_core::cas::{clear_workspace_cas, workspace_cas_status};

/// Errors while managing the discovery cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("task {name} not found")]
    TaskNotFound { name: String },
}

impl CacheError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) | Self::Json(_) | Self::Prepare(_) | Self::Flake(_) => exit::EVALUATION,
            Self::Plan(_) | Self::TaskNotFound { .. } => exit::NOT_FOUND,
        }
    }
}

#[derive(Serialize)]
struct CacheClearJson {
    discovery_removed: usize,
    capabilities_removed: usize,
    workspace_removed: usize,
}

#[derive(Serialize)]
struct CacheStatusSection {
    path: String,
    entries: usize,
    total_bytes: u64,
}

#[derive(Serialize)]
struct CacheStatusJson {
    discovery: CacheStatusSection,
    capabilities: CacheStatusSection,
    workspace: CacheStatusSection,
}

/// Remove all discovery cache entries.
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn clear(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery_removed = clear_discovery_cache()?;
    let capabilities_removed = clear_capability_cache()?;
    let workspace_removed = clear_workspace_cas()?;
    if json {
        let payload = CacheClearJson {
            discovery_removed,
            capabilities_removed,
            workspace_removed,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        runner
            .info(format!(
                "removed {discovery_removed} discovery cache entr{}, {capabilities_removed} capability cache entr{}, and {workspace_removed} workspace CAS entr{}",
                if discovery_removed == 1 { "y" } else { "ies" },
                if capabilities_removed == 1 { "y" } else { "ies" },
                if workspace_removed == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}

/// Print discovery cache location and size.
///
/// # Errors
///
/// Returns [`CacheError`] when the cache directory cannot be read or output fails.
pub fn status(json: bool, mut runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery = discovery_cache_status()?;
    let capabilities = capability_cache_status()?;
    let workspace = workspace_cas_status()?;
    if json {
        let payload = CacheStatusJson {
            discovery: CacheStatusSection {
                path: discovery.path,
                entries: discovery.entries,
                total_bytes: discovery.total_bytes,
            },
            capabilities: CacheStatusSection {
                path: capabilities.path,
                entries: capabilities.entries,
                total_bytes: capabilities.total_bytes,
            },
            workspace: CacheStatusSection {
                path: workspace.path,
                entries: workspace.entries,
                total_bytes: workspace.total_bytes,
            },
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        render_status_section(
            &mut runner,
            "discovery",
            &discovery.path,
            discovery.entries,
            discovery.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "capability",
            &capabilities.path,
            capabilities.entries,
            capabilities.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "workspace",
            &workspace.path,
            workspace.entries,
            workspace.total_bytes,
        )?;
    }
    Ok(())
}

/// Explain workspace CAS key material and hit/miss for a task.
///
/// # Errors
///
/// Returns [`CacheError`] when discovery, planning, or output fails.
pub fn explain_task(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    task: &str,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let snapshot = WorkspaceSnapshot::load(flake_arg, nix_override, true, &Default::default())?;
    let document = snapshot
        .tasks
        .as_ref()
        .ok_or_else(|| CacheError::TaskNotFound {
            name: task.to_owned(),
        })?;
    let canonical = resolve_task_name(document, task)
        .map_err(|error| CacheError::TaskNotFound { name: error.name })?
        .to_owned();
    let plan = build_execution_plan_roots(
        &document.tasks,
        &[canonical.as_str()],
        nxr_task::FailurePolicy::FailFast,
        None,
    )?;
    snapshot
        .validate_task_apps(document)
        .map_err(PrepareError::NotFound)?;
    let prepared = snapshot.prepare_task_nodes(
        document,
        &[canonical.clone()],
        &plan.serial_order,
        &[],
        false,
        None,
        None,
        crate::shell_mode::ShellMode::Smart,
        &nxr_core::EnvironmentPolicy::Inherit,
        &Default::default(),
        None,
    )?;
    let node = prepared
        .get(&canonical)
        .ok_or_else(|| CacheError::TaskNotFound {
            name: canonical.clone(),
        })?;
    let explain = explain_workspace_cache(node);
    if json {
        let rendered = serde_json::to_string_pretty(&explain)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        render_cache_explain(&explain, &canonical, runner)?;
    }
    Ok(())
}

fn render_cache_explain(
    explain: &CacheExplain,
    task: &str,
    mut runner: RunnerOutput,
) -> Result<(), CacheError> {
    runner
        .info(format!("cache explain for task {task}"))
        .map_err(CacheError::Io)?;
    runner
        .info(format!("  tier: {:?}", explain.tier))
        .map_err(CacheError::Io)?;
    runner
        .info(format!("  cache_enabled: {}", explain.cache_enabled))
        .map_err(CacheError::Io)?;
    if let Some(key) = &explain.action_key {
        runner
            .info(format!("  action_key: {key}"))
            .map_err(CacheError::Io)?;
    }
    runner
        .info(format!("  lookup: {:?}", explain.lookup))
        .map_err(CacheError::Io)?;
    if !explain.key_components.is_empty() {
        runner
            .info("  key_components:".to_owned())
            .map_err(CacheError::Io)?;
        for (label, value) in &explain.key_components {
            runner
                .info(format!("    {label}: {value}"))
                .map_err(CacheError::Io)?;
        }
    }
    Ok(())
}

fn render_status_section(
    runner: &mut RunnerOutput,
    label: &str,
    path: &str,
    entries: usize,
    total_bytes: u64,
) -> Result<(), CacheError> {
    if path.is_empty() {
        runner
            .info(format!("{label} cache unavailable on this host"))
            .map_err(CacheError::Io)?;
    } else {
        runner
            .info(format!(
                "{label} cache: {path} ({entries} entr{}, {total_bytes} bytes)",
                if entries == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}
