//! `nxr affected` command and shared analysis helpers for `--affected` execution.

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
use nxr_task::{ResolveTaskError, TaskDocument, resolve_task_name};

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while running the affected command or resolving `--affected` roots.
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
    #[error(transparent)]
    TaskNotFound(#[from] ResolveTaskError),
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
            Self::TaskNotFound(_) => nxr_core::diagnostics::exit::NOT_FOUND,
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

impl AffectedPathSources {
    /// True when at least one git-backed collector is enabled.
    #[must_use]
    pub const fn needs_git(&self) -> bool {
        self.base.is_some() || self.working_tree || self.all_changes.is_some()
    }
}

/// Result of discovering a flake and classifying affected operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffectedSelection {
    pub analysis: AffectedAnalysis,
    pub document: nxr_task::TaskDocument,
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
    let selection = select_for_flake(
        flake_arg,
        nix_override,
        refresh_discovery,
        nix_flags,
        sources,
        strict,
        paths,
        runner,
    )?;

    if json {
        write_json(&mut io::stdout().lock(), &selection.analysis)?;
    } else {
        write_human(&mut io::stdout().lock(), &selection.analysis)?;
    }

    Ok(())
}

/// Run affected analysis for a flake (shared by `affected`, `task --affected`, `plan --affected`).
///
/// # Errors
///
/// Returns [`AffectedCommandError`] when path collection, discovery, or analysis fails.
#[allow(clippy::too_many_arguments)]
pub fn select_for_flake(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    sources: &AffectedPathSources,
    strict: bool,
    paths: &[String],
    runner: RunnerOutput,
) -> Result<AffectedSelection, AffectedCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;
    let changed_paths = collect_changed_paths(&flake, sources, paths)?;

    runner
        .info(format!(
            "analyzing {} changed path(s) for {} (strict={strict})",
            changed_paths.len(),
            flake.display
        ))
        .map_err(AffectedCommandError::Io)?;

    let workspace = discover_workspace(&flake, &adapter, refresh_discovery, nix_flags)?;
    let document = workspace
        .tasks
        .expect("affected always discovers tasks with apps");
    let graph = build_graph(&workspace.apps, &document);
    let analysis = analyze(
        &graph,
        &changed_paths,
        &flake.display,
        &adapter.system,
        strict,
    );
    Ok(AffectedSelection { analysis, document })
}

/// Resolve task roots for `--affected` execution or planning.
///
/// When `requested` is empty, returns every task in the analysis lists (affected,
/// plus unknown under strict). When `requested` is non-empty, resolves aliases and
/// intersects with that set (unknown names error).
///
/// # Errors
///
/// Returns [`AffectedCommandError::TaskNotFound`] when a requested name is missing.
pub fn resolve_affected_task_roots(
    document: &TaskDocument,
    analysis: &AffectedAnalysis,
    requested: &[String],
) -> Result<Vec<String>, AffectedCommandError> {
    let candidates: BTreeSet<String> = analysis.tasks.iter().cloned().collect();

    if requested.is_empty() {
        return Ok(analysis.tasks.clone());
    }

    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    for name in requested {
        let canonical = resolve_task_name(document, name)?.to_owned();
        if candidates.contains(&canonical) && seen.insert(canonical.clone()) {
            roots.push(canonical);
        }
    }
    Ok(roots)
}

/// Collect and validate changed paths from CLI sources.
///
/// # Errors
///
/// Returns [`AffectedCommandError::Usage`] when no path source is given or paths
/// are invalid, and [`AffectedCommandError::Git`] on git failures.
pub fn collect_changed_paths(
    flake: &FlakeSelection,
    sources: &AffectedPathSources,
    paths: &[String],
) -> Result<Vec<String>, AffectedCommandError> {
    let mut changed_paths = paths.to_vec();
    for (index, path) in changed_paths.iter().enumerate() {
        nxr_core::validate_repo_relative_path(&format!("paths[{index}]"), path)
            .map_err(|error| AffectedCommandError::Usage(error.to_string()))?;
    }

    if sources.needs_git() {
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

    Ok(dedupe_paths(changed_paths))
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
                ..Default::default()
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

#[cfg(test)]
mod tests {
    use super::resolve_affected_task_roots;
    use nxr_affected::{AffectedAnalysis, AffectedNode, NodeStatus};
    use nxr_task::{TaskDefinition, TaskDocument};

    fn doc() -> TaskDocument {
        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert("shared-lib".to_owned(), TaskDefinition::new("shared-check"));
        let mut api = TaskDefinition::new("api-test");
        api.depends_on = vec!["shared-lib".to_owned()];
        tasks.insert("api-test".to_owned(), api);
        TaskDocument::new(tasks)
    }

    fn analysis(tasks: &[&str]) -> AffectedAnalysis {
        AffectedAnalysis {
            schema_version: AffectedAnalysis::SCHEMA_VERSION,
            flake: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            strict: true,
            changed_paths: vec!["shared/lib.txt".to_owned()],
            apps: Vec::new(),
            tasks: tasks.iter().map(|name| (*name).to_owned()).collect(),
            nodes: tasks
                .iter()
                .map(|name| AffectedNode {
                    kind: "task".to_owned(),
                    name: (*name).to_owned(),
                    status: NodeStatus::Affected,
                    reasons: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn resolve_roots_uses_all_candidates_when_unrequested() {
        let roots =
            resolve_affected_task_roots(&doc(), &analysis(&["api-test", "shared-lib"]), &[])
                .expect("roots");
        assert_eq!(roots, vec!["api-test".to_owned(), "shared-lib".to_owned()]);
    }

    #[test]
    fn resolve_roots_intersects_requested_names() {
        let roots = resolve_affected_task_roots(
            &doc(),
            &analysis(&["api-test", "shared-lib"]),
            &["api-test".to_owned()],
        )
        .expect("roots");
        assert_eq!(roots, vec!["api-test".to_owned()]);
    }

    #[test]
    fn resolve_roots_skips_requested_unaffected() {
        let roots = resolve_affected_task_roots(
            &doc(),
            &analysis(&["shared-lib"]),
            &["api-test".to_owned()],
        )
        .expect("roots");
        assert!(roots.is_empty());
    }
}
