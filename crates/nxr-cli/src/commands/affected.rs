//! `nxr affected` command implementation.

use std::collections::BTreeSet;
use std::io::{self, Write};

use nxr_affected::{
    AffectedAnalysis, AffectedError, GitDiffError, analyze, build_graph, git_diff_name_only,
};
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while running the affected command.
#[derive(Debug, thiserror::Error)]
pub enum AffectedCommandError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Analysis(#[from] AffectedError),
    #[error(transparent)]
    Git(#[from] GitDiffError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Usage(String),
}

impl AffectedCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Analysis(_) | Self::Git(_) | Self::Usage(_) => nxr_core::diagnostics::exit::USAGE,
            Self::Io(_) => nxr_core::diagnostics::exit::EVALUATION,
        }
    }
}

/// Discover affected apps and tasks for the given changed paths.
///
/// # Errors
///
/// Returns [`AffectedCommandError`] when discovery or analysis fails.
#[allow(clippy::too_many_arguments)]
pub fn run(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    base: Option<&str>,
    paths: &[String],
    runner: RunnerOutput,
) -> Result<(), AffectedCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;

    let mut changed_paths = paths.to_vec();
    if let Some(base_ref) = base {
        let local_root = flake.local_root.as_ref().ok_or_else(|| {
            AffectedCommandError::Usage(
                "--base requires a local flake root (remote flakes are unsupported)".to_owned(),
            )
        })?;
        let git_paths = git_diff_name_only(local_root, base_ref)?;
        changed_paths.extend(git_paths);
    }

    changed_paths = dedupe_paths(changed_paths);

    runner
        .info(format!(
            "analyzing {} changed path(s) for {}",
            changed_paths.len(),
            flake.display
        ))
        .map_err(AffectedCommandError::Io)?;

    let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
    let task_doc = workspace
        .tasks
        .expect("affected always discovers tasks with apps");
    let graph = build_graph(&workspace.apps, &task_doc);
    let analysis = analyze(&graph, &changed_paths, &flake.display, &adapter.system)?;

    if json {
        write_json(&mut io::stdout().lock(), &analysis)?;
    } else {
        write_human(&mut io::stdout().lock(), &analysis)?;
    }

    Ok(())
}

fn dedupe_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            deduped.push(path);
        }
    }
    deduped
}

fn discover_workspace(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<WorkspaceDiscovery, AffectedCommandError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
    };
    let flake_ref = flake.nix_ref.clone();

    discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions::with_tasks(refresh_discovery),
        || {
            let apps = adapter
                .discover_apps(&flake_ref, nix_flags)
                .map_err(AffectedCommandError::Nix)?;
            let tasks = adapter
                .discover_tasks(&flake_ref, nix_flags)
                .map_err(AffectedCommandError::Tasks)?;
            Ok(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
            })
        },
    )
}

fn write_json(writer: &mut impl Write, analysis: &AffectedAnalysis) -> io::Result<()> {
    let json = serde_json::to_string_pretty(analysis)?;
    writeln!(writer, "{json}")
}

fn write_human(writer: &mut impl Write, analysis: &AffectedAnalysis) -> io::Result<()> {
    writeln!(
        writer,
        "Affected operations for {} ({})",
        analysis.flake, analysis.system
    )?;
    writeln!(writer)?;
    writeln!(writer, "Changed paths:")?;
    for path in &analysis.changed_paths {
        writeln!(writer, "  {}", sanitize_terminal_text(path))?;
    }
    writeln!(writer)?;

    if analysis.apps.is_empty() && analysis.tasks.is_empty() {
        writeln!(writer, "No affected apps or tasks.")?;
        return Ok(());
    }

    if !analysis.apps.is_empty() {
        writeln!(writer, "Apps:")?;
        for name in &analysis.apps {
            writeln!(writer, "  {}", sanitize_terminal_text(name))?;
        }
        writeln!(writer)?;
    }

    if !analysis.tasks.is_empty() {
        writeln!(writer, "Tasks:")?;
        for name in &analysis.tasks {
            writeln!(writer, "  {}", sanitize_terminal_text(name))?;
        }
    }

    Ok(())
}
