//! `nxr affected` command and shared analysis helpers for `--affected` execution.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

use nxr_affected::{
    AffectedAnalysis, GitDiffError, MemberAffectedAnalysis, MemberChange, MemberEdge, MemberNode,
    NodeStatus, analyze, analyze_members, build_graph, git_all_changes, git_diff_name_only,
    git_working_tree_changes,
};
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_task::{
    ResolveTaskError, TaskDocument, WorkflowDocument, WorkflowError, WorkflowTask,
    load_workflow_document, resolve_task_name,
};

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
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
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
            Self::Workflow(error) => error.exit_code(),
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

/// Which analyzer to invoke for a set of explicit changed paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AffectedAnalyzer {
    /// Single-checkout [`analyze`].
    Analyze,
    /// Member-qualified [`analyze_members`] with these parsed changes.
    AnalyzeMembers(Vec<MemberChange>),
}

/// Choose [`analyze`] versus [`analyze_members`] from explicit paths and known members.
///
/// A path is member-qualified only when it has the form `MEMBER:RELATIVE` and
/// `MEMBER` is listed in `known_source_members`. Relative may be empty or
/// `REVISION`. Mixing plain and member-qualified paths is a usage error.
///
/// # Errors
///
/// Returns [`AffectedCommandError::Usage`] on a mixed set or an invalid relative
/// segment after a known member prefix.
pub fn choose_affected_analyzer(
    paths: &[String],
    known_source_members: &BTreeSet<String>,
) -> Result<AffectedAnalyzer, AffectedCommandError> {
    if paths.is_empty() {
        return Ok(AffectedAnalyzer::Analyze);
    }

    let mut member_changes = Vec::new();
    let mut plain = 0usize;
    let mut member = 0usize;

    for path in paths {
        match parse_member_qualified_path(path, known_source_members)? {
            Some(change) => {
                member += 1;
                member_changes.push(change);
            }
            None => plain += 1,
        }
    }

    if member > 0 && plain > 0 {
        return Err(AffectedCommandError::Usage(
            "mixed plain and member-qualified paths are not supported".to_owned(),
        ));
    }
    if member > 0 {
        Ok(AffectedAnalyzer::AnalyzeMembers(member_changes))
    } else {
        Ok(AffectedAnalyzer::Analyze)
    }
}

/// Parse `MEMBER:RELATIVE` when `MEMBER` is a known source member.
///
/// Returns `Ok(None)` when the path is not member-qualified (plain path, or a
/// colon form whose left side is not a known source member).
fn parse_member_qualified_path(
    path: &str,
    known_source_members: &BTreeSet<String>,
) -> Result<Option<MemberChange>, AffectedCommandError> {
    let Some((member, relative)) = path.split_once(':') else {
        return Ok(None);
    };
    if member.is_empty() || !known_source_members.contains(member) {
        return Ok(None);
    }
    if !relative.is_empty() && relative != "REVISION" {
        nxr_core::validate_repo_relative_path("member-qualified path", relative)
            .map_err(|error| AffectedCommandError::Usage(error.to_string()))?;
    }
    Ok(Some(MemberChange {
        member: member.to_owned(),
        path: relative.to_owned(),
    }))
}

/// Source-member names declared on workflow tasks.
#[must_use]
pub fn known_source_members(document: &WorkflowDocument) -> BTreeSet<String> {
    document
        .tasks
        .values()
        .filter_map(|task| task.source_member.clone())
        .collect()
}

/// Build member nodes and declared edges from a workflow document.
///
/// Nodes come from tasks with `source_member` (`path_roots` = `task.inputs`).
/// Edges come from `depends_on`: an existing task key wins over colon splitting.
/// One edge is emitted per source-task input as `source_path_prefix`; a source
/// task with no inputs (or an unresolved source) yields one edge with `None`.
#[must_use]
pub fn member_graph_from_workflow(
    document: &WorkflowDocument,
) -> (Vec<MemberNode>, Vec<MemberEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for (name, task) in &document.tasks {
        let Some(consumer_member) = task.source_member.as_deref() else {
            continue;
        };
        nodes.push(MemberNode {
            member: consumer_member.to_owned(),
            name: name.clone(),
            path_roots: task.inputs.clone(),
        });

        for dep in &task.depends_on {
            let Some((source_member, source_task)) = resolve_member_dependency(dep, document)
            else {
                continue;
            };
            let prefixes: Vec<Option<String>> = match source_task {
                Some(source) if !source.inputs.is_empty() => source
                    .inputs
                    .iter()
                    .map(|input| Some(input.clone()))
                    .collect(),
                _ => vec![None],
            };
            for source_path_prefix in prefixes {
                edges.push(MemberEdge {
                    consumer_member: consumer_member.to_owned(),
                    consumer_name: name.clone(),
                    source_member: source_member.clone(),
                    source_path_prefix,
                });
            }
        }
    }

    nodes.sort_by(|left, right| {
        left.member
            .cmp(&right.member)
            .then(left.name.cmp(&right.name))
    });
    (nodes, edges)
}

/// Resolve a `depends_on` edge to `(source_member, optional source task)`.
///
/// Existing task keys win over `member:task` splitting. Returns `None` when the
/// dependency is a local key whose task has no `source_member`.
fn resolve_member_dependency<'a>(
    dep: &str,
    document: &'a WorkflowDocument,
) -> Option<(String, Option<&'a WorkflowTask>)> {
    if let Some(task) = document.tasks.get(dep) {
        let member = task.source_member.clone()?;
        return Some((member, Some(task)));
    }

    let (member, task_name) = dep.split_once(':')?;
    if member.is_empty() || task_name.is_empty() {
        return None;
    }

    if let Some(task) = document.tasks.get(task_name)
        && task.source_member.as_deref() == Some(member)
    {
        return Some((member.to_owned(), Some(task)));
    }

    // Unknown source member (or missing task): still emit an edge for coverage.
    Some((member.to_owned(), None))
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
    let workflow = load_workflow_for_flake(&flake)?;
    let known_members = workflow
        .as_ref()
        .map(known_source_members)
        .unwrap_or_default();
    let analyzer = choose_affected_analyzer(paths, &known_members)?;

    match analyzer {
        AffectedAnalyzer::AnalyzeMembers(changes) => {
            if sources.needs_git() {
                return Err(AffectedCommandError::Usage(
                    "member-qualified paths cannot be combined with --base / --working-tree / --all-changes"
                        .to_owned(),
                ));
            }
            let document = workflow.ok_or_else(|| {
                AffectedCommandError::Usage(
                    "member-qualified paths require `.nxr/workflows.toml` at the flake root"
                        .to_owned(),
                )
            })?;
            let (nodes, edges) = member_graph_from_workflow(&document);
            runner
                .info(format!(
                    "analyzing {} member-qualified change(s) for {} via analyze_members",
                    changes.len(),
                    flake.display
                ))
                .map_err(AffectedCommandError::Io)?;
            let analysis = analyze_members(&nodes, &edges, &changes);
            if json {
                write_member_json(&mut io::stdout().lock(), &analysis)?;
            } else {
                write_member_human(&mut io::stdout().lock(), &analysis)?;
            }
            Ok(())
        }
        AffectedAnalyzer::Analyze => {
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
    }
}

/// Run affected analysis for a flake (shared by `affected`, `task --affected`, `plan --affected`).
///
/// Keeps the single-checkout [`analyze`] call. Member-qualified explicit paths
/// are a usage error here (use `nxr affected`).
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
    let workflow = load_workflow_for_flake(&flake)?;
    let known_members = workflow
        .as_ref()
        .map(known_source_members)
        .unwrap_or_default();
    match choose_affected_analyzer(paths, &known_members)? {
        AffectedAnalyzer::AnalyzeMembers(_) => {
            return Err(AffectedCommandError::Usage(
                "member-qualified paths are not supported with task/plan --affected; use `nxr affected`"
                    .to_owned(),
            ));
        }
        AffectedAnalyzer::Analyze => {}
    }

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

fn load_workflow_for_flake(
    flake: &FlakeSelection,
) -> Result<Option<WorkflowDocument>, AffectedCommandError> {
    let Some(local_root) = flake.local_root.as_ref() else {
        return Ok(None);
    };
    Ok(load_workflow_document(Path::new(local_root.as_str()))?)
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

fn write_member_json(writer: &mut impl Write, analysis: &MemberAffectedAnalysis) -> io::Result<()> {
    let json = serde_json::to_string_pretty(analysis)?;
    writeln!(writer, "{json}")
}

fn write_member_human(
    writer: &mut impl Write,
    analysis: &MemberAffectedAnalysis,
) -> io::Result<()> {
    for node in &analysis.nodes {
        writeln!(
            writer,
            "{} {} {}",
            sanitize_terminal_text(&node.member),
            sanitize_terminal_text(&node.name),
            status_label(node.status)
        )?;
    }
    Ok(())
}

fn status_label(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Affected => "affected",
        NodeStatus::Unaffected => "unaffected",
        NodeStatus::Unknown => "unknown",
    }
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
    use super::{
        AffectedAnalyzer, choose_affected_analyzer, known_source_members,
        member_graph_from_workflow, parse_member_qualified_path, resolve_affected_task_roots,
    };
    use nxr_affected::{AffectedAnalysis, AffectedNode, NodeStatus, analyze_members};
    use nxr_core::Operation;
    use nxr_task::{
        NodeResources, TaskDefinition, TaskDocument, WorkflowDocument, WorkflowPlatform,
        WorkflowTask,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn doc() -> TaskDocument {
        let mut tasks = BTreeMap::new();
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

    fn workflow_task(
        source_member: Option<&str>,
        depends_on: &[&str],
        inputs: &[&str],
    ) -> WorkflowTask {
        WorkflowTask {
            depends_on: depends_on.iter().map(|name| (*name).to_owned()).collect(),
            platform: WorkflowPlatform::Any,
            architecture: None,
            resources: NodeResources::EMPTY,
            environment: BTreeMap::new(),
            working_directory: None,
            artifacts: Vec::new(),
            inputs: inputs.iter().map(|name| (*name).to_owned()).collect(),
            operation: Operation::NativeCommand {
                argv: vec!["/bin/true".to_owned()],
            },
            source_member: source_member.map(str::to_owned),
            remote_eligible: false,
        }
    }

    fn lattice_stargate_workflow() -> WorkflowDocument {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "typecheck".to_owned(),
            workflow_task(Some("stargate"), &[], &["src"]),
        );
        tasks.insert(
            "build-integration".to_owned(),
            workflow_task(Some("lattice"), &["typecheck"], &["crates/lattice"]),
        );
        tasks.insert(
            "unrelated-lint".to_owned(),
            workflow_task(Some("lattice"), &[], &["crates/lint"]),
        );
        tasks.insert(
            "cell-check".to_owned(),
            workflow_task(Some("cell"), &[], &["src"]),
        );
        WorkflowDocument {
            schema_version: 1,
            tasks,
        }
    }

    fn status_of(
        result: &nxr_affected::MemberAffectedAnalysis,
        member: &str,
        name: &str,
    ) -> NodeStatus {
        result
            .nodes
            .iter()
            .find(|node| node.member == member && node.name == name)
            .expect("node present")
            .status
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

    #[test]
    fn plain_path_is_not_member_qualified() {
        let members: BTreeSet<String> = ["stargate".to_owned()].into_iter().collect();
        assert_eq!(
            parse_member_qualified_path("src/lib.rs", &members).expect("parse"),
            None
        );
        assert_eq!(
            choose_affected_analyzer(&["src/lib.rs".to_owned()], &members).expect("choose"),
            AffectedAnalyzer::Analyze
        );
    }

    #[test]
    fn member_qualified_path_selects_analyze_members() {
        let members: BTreeSet<String> = ["stargate".to_owned()].into_iter().collect();
        match choose_affected_analyzer(&["stargate:src/lib.rs".to_owned()], &members)
            .expect("choose")
        {
            AffectedAnalyzer::AnalyzeMembers(changes) => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].member, "stargate");
                assert_eq!(changes[0].path, "src/lib.rs");
            }
            AffectedAnalyzer::Analyze => panic!("expected analyze_members"),
        }
    }

    #[test]
    fn mixed_plain_and_member_paths_are_usage_error() {
        let members: BTreeSet<String> = ["stargate".to_owned()].into_iter().collect();
        let error = choose_affected_analyzer(
            &["stargate:src/lib.rs".to_owned(), "crates/foo.rs".to_owned()],
            &members,
        )
        .expect_err("mix");
        assert!(error.to_string().contains("mixed"));
    }

    #[test]
    fn stargate_path_selects_stargate_and_matching_lattice_edge_only() {
        let document = lattice_stargate_workflow();
        let known = known_source_members(&document);
        let analyzer =
            choose_affected_analyzer(&["stargate:src/lib.rs".to_owned()], &known).expect("choose");
        let AffectedAnalyzer::AnalyzeMembers(changes) = analyzer else {
            panic!("expected member analyzer");
        };
        let (nodes, edges) = member_graph_from_workflow(&document);
        let result = analyze_members(&nodes, &edges, &changes);

        assert_eq!(result.coverage, "complete");
        assert_eq!(
            status_of(&result, "stargate", "typecheck"),
            NodeStatus::Affected
        );
        assert_eq!(
            status_of(&result, "lattice", "build-integration"),
            NodeStatus::Affected
        );
        assert_eq!(
            status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
        assert_eq!(
            status_of(&result, "cell", "cell-check"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn edge_without_prefix_marks_consumer_unknown_not_blanket() {
        let mut document = lattice_stargate_workflow();
        document.tasks.insert(
            "typecheck".to_owned(),
            workflow_task(Some("stargate"), &[], &[]),
        );
        let (nodes, edges) = member_graph_from_workflow(&document);
        assert!(
            edges
                .iter()
                .any(|edge| edge.consumer_name == "build-integration"
                    && edge.source_path_prefix.is_none())
        );
        let result = analyze_members(
            &nodes,
            &edges,
            &[nxr_affected::MemberChange {
                member: "stargate".to_owned(),
                path: "src/lib.rs".to_owned(),
            }],
        );
        assert_eq!(result.coverage, "partial");
        assert_eq!(
            status_of(&result, "lattice", "build-integration"),
            NodeStatus::Unknown
        );
        assert_eq!(
            status_of(&result, "lattice", "unrelated-lint"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn unknown_source_member_edge_sets_unknown_coverage() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "typecheck".to_owned(),
            workflow_task(Some("stargate"), &[], &["src"]),
        );
        tasks.insert(
            "build-integration".to_owned(),
            workflow_task(Some("lattice"), &["missing:leaf"], &["crates/lattice"]),
        );
        let document = WorkflowDocument {
            schema_version: 1,
            tasks,
        };
        let (nodes, edges) = member_graph_from_workflow(&document);
        assert!(
            edges
                .iter()
                .any(|edge| edge.source_member == "missing" && edge.source_path_prefix.is_none())
        );
        let result = analyze_members(
            &nodes,
            &edges,
            &[nxr_affected::MemberChange {
                member: "stargate".to_owned(),
                path: "src/lib.rs".to_owned(),
            }],
        );
        assert_eq!(result.coverage, "unknown");
        assert_eq!(
            status_of(&result, "lattice", "build-integration"),
            NodeStatus::Unaffected
        );
    }

    #[test]
    fn existing_task_key_wins_over_colon_splitting() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "lattice:test".to_owned(),
            workflow_task(Some("stargate"), &[], &["src"]),
        );
        tasks.insert(
            "build".to_owned(),
            workflow_task(Some("lattice"), &["lattice:test"], &["crates/lattice"]),
        );
        let document = WorkflowDocument {
            schema_version: 1,
            tasks,
        };
        let (_nodes, edges) = member_graph_from_workflow(&document);
        let edge = edges
            .iter()
            .find(|edge| edge.consumer_name == "build")
            .expect("edge");
        assert_eq!(edge.source_member, "stargate");
        assert_eq!(edge.source_path_prefix.as_deref(), Some("src"));
    }
}
