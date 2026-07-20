//! `nxr affected` command implementation.

use std::collections::BTreeSet;
use std::io::{self, Write};

use nxr_affected::{
    AffectedAnalysis, GitDiffError, NodeStatus, analyze, build_graph, git_all_changes,
    git_diff_name_only, git_working_tree_changes,
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
            Self::Git(_) | Self::Usage(_) => nxr_core::diagnostics::exit::USAGE,
            Self::Io(_) => nxr_core::diagnostics::exit::EVALUATION,
        }
    }
}

/// How changed paths were collected for analysis.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AffectedPathSources {
    /// `git diff <base>...HEAD` ref, when requested.
    pub base: Option<String>,
    /// Include unstaged, staged, and untracked working-tree paths.
    pub working_tree: bool,
    /// Shorthand for base range union working tree.
    pub all_changes: Option<String>,
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
    sources: &AffectedPathSources,
    strict: bool,
    paths: &[String],
    runner: RunnerOutput,
) -> Result<(), AffectedCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;

    let mut changed_paths = paths.to_vec();
    for (index, path) in changed_paths.iter().enumerate() {
        nxr_core::validate_repo_relative_path(&format!("paths[{index}]"), path)
            .map_err(|error| AffectedCommandError::Usage(error.to_string()))?;
    }
    let needs_git = sources.base.is_some() || sources.working_tree || sources.all_changes.is_some();
    if needs_git {
        let local_root = flake.local_root.as_ref().ok_or_else(|| {
            AffectedCommandError::Usage(
                "git path collection (--base / --working-tree / --all-changes) requires a local flake root (remote flakes are unsupported)".to_owned(),
            )
        })?;

        if let Some(ref_name) = &sources.all_changes {
            changed_paths.extend(git_all_changes(local_root, ref_name)?);
        } else {
            if let Some(base_ref) = &sources.base {
                changed_paths.extend(git_diff_name_only(local_root, base_ref)?);
            }
            if sources.working_tree {
                changed_paths.extend(git_working_tree_changes(local_root)?);
            }
        }
    }

    changed_paths = dedupe_paths(changed_paths);

    let has_path_source = !paths.is_empty()
        || sources.base.is_some()
        || sources.working_tree
        || sources.all_changes.is_some();
    if !has_path_source {
        return Err(AffectedCommandError::Usage(
            "no path source specified; pass paths as arguments or use --base / --working-tree / --all-changes"
                .to_owned(),
        ));
    }

    runner
        .info(format!(
            "analyzing {} changed path(s) for {} (strict={strict})",
            changed_paths.len(),
            flake.display
        ))
        .map_err(AffectedCommandError::Io)?;

    let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
    let task_doc = workspace
        .tasks
        .expect("affected always discovers tasks with apps");
    let graph = build_graph(&workspace.apps, &task_doc);
    let analysis = analyze(
        &graph,
        &changed_paths,
        &flake.display,
        &adapter.system,
        strict,
    );

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
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
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
        "Affected operations for {} ({}) [strict={}]",
        analysis.flake, analysis.system, analysis.strict
    )?;
    writeln!(writer)?;
    writeln!(writer, "Changed paths:")?;
    for path in &analysis.changed_paths {
        writeln!(writer, "  {}", sanitize_terminal_text(path))?;
    }
    writeln!(writer)?;

    let affected_apps: Vec<_> = analysis
        .nodes
        .iter()
        .filter(|node| node.kind == "app" && node.status == NodeStatus::Affected)
        .map(|node| node.name.as_str())
        .collect();
    let unknown_apps: Vec<_> = analysis
        .nodes
        .iter()
        .filter(|node| node.kind == "app" && node.status == NodeStatus::Unknown)
        .map(|node| node.name.as_str())
        .collect();
    let affected_tasks: Vec<_> = analysis
        .nodes
        .iter()
        .filter(|node| node.kind == "task" && node.status == NodeStatus::Affected)
        .map(|node| node.name.as_str())
        .collect();
    let unknown_tasks: Vec<_> = analysis
        .nodes
        .iter()
        .filter(|node| node.kind == "task" && node.status == NodeStatus::Unknown)
        .map(|node| node.name.as_str())
        .collect();

    if affected_apps.is_empty()
        && unknown_apps.is_empty()
        && affected_tasks.is_empty()
        && unknown_tasks.is_empty()
    {
        writeln!(writer, "No affected or unknown apps or tasks.")?;
        return Ok(());
    }

    write_section(writer, "Apps (affected)", &affected_apps)?;
    write_section(writer, "Apps (unknown)", &unknown_apps)?;
    write_section(writer, "Tasks (affected)", &affected_tasks)?;
    write_section(writer, "Tasks (unknown)", &unknown_tasks)?;

    if analysis.strict {
        writeln!(
            writer,
            "Strict policy: apps/tasks lists include unknown (only unaffected is skippable)."
        )?;
    } else {
        writeln!(
            writer,
            "Non-strict policy: apps/tasks lists omit unknown; nodes includes the full classification."
        )?;
    }

    Ok(())
}

fn write_section(writer: &mut impl Write, title: &str, names: &[&str]) -> io::Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    writeln!(writer, "{title}:")?;
    for name in names {
        writeln!(writer, "  {}", sanitize_terminal_text(name))?;
    }
    writeln!(writer)?;
    Ok(())
}
