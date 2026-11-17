//! `nxr op` and `nxr env explain`.
//!
//! Both commands call [`nxr_task::catalog`] and do not run the operation.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use camino::Utf8Path;
use nxr_nix::OptionalNixFlags;
use nxr_task::{
    Declaration, OperationKind, OperationRecord, SourceSetV2, WorkspaceInstance, describe,
    discover_members, explain, operation_record, plan, search, workspace_instance_from_checkout,
};
use serde_json::Value;

use crate::cli::{EnvCommand, OpCommand};
use crate::commands::common::build_adapter;
use crate::commands::script::SCRIPT_CONVENTION_DIR;
use crate::flake::resolve_flake;
use crate::runner_output::RunnerOutput;

/// Local checkouts for a SourceSet v2 document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocators {
    /// Checkout for the primary member.
    pub primary: PathBuf,
    /// Sibling checkouts keyed by member `logical_path`.
    pub siblings: BTreeMap<String, PathBuf>,
}

/// Ids returned for a query. The CLI prints these same ids.
#[cfg(test)]
#[must_use]
pub fn matching_ids(records: &[OperationRecord], query: &str) -> Vec<String> {
    search(records, query)
        .into_iter()
        .map(|record| record.operation_id.clone())
        .collect()
}

/// Run `nxr op`.
///
/// # Errors
///
/// Returns an error when discovery fails or the id is unknown.
pub fn run_op(
    flake: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    nix_flags: &OptionalNixFlags,
    command: &OpCommand,
    source_set: Option<&Path>,
    source_locator: Option<&str>,
    runner: RunnerOutput,
) -> Result<i32, String> {
    let records = load_records_for_command(
        flake,
        nix_override,
        nix_flags,
        source_set,
        source_locator,
        runner,
    )?;
    match command {
        OpCommand::Search { query } => {
            let matched = search(&records, query);
            write_json_or_lines(json, &matched, |record| record.operation_id.clone())?;
        }
        OpCommand::Describe { id } => {
            let record = describe(&records, id).ok_or_else(|| format!("unknown operation {id}"))?;
            write_json_or_lines(json, std::slice::from_ref(record), |record| {
                record.operation_id.clone()
            })?;
        }
        OpCommand::Plan { id } => {
            let record = describe(&records, id).ok_or_else(|| format!("unknown operation {id}"))?;
            let planned = plan(record);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&planned).map_err(|err| err.to_string())?
                );
            } else {
                println!(
                    "{} {} allowed={}",
                    planned.operation_id, planned.inferred_effect, planned.execution_allowed
                );
            }
        }
    }
    Ok(0)
}

/// Run `nxr env explain`.
///
/// # Errors
///
/// Returns an error when the id is unknown.
pub fn run_env(
    flake: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    nix_flags: &OptionalNixFlags,
    command: &EnvCommand,
    runner: RunnerOutput,
) -> Result<i32, String> {
    let EnvCommand::Explain { id } = command;
    let records = load_records(flake, nix_override, nix_flags, runner)?;
    let record = describe(&records, id).ok_or_else(|| format!("unknown operation {id}"))?;
    let report = explain(record, &std::env::var("PATH").unwrap_or_default());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?
        );
    } else if report.mismatches.is_empty() {
        println!(
            "{} ambient {} resolved {}",
            report.operation_id, report.ambient.compiler, report.resolved.compiler
        );
    } else {
        println!(
            "{} compiler mismatch: {}",
            report.operation_id, report.mismatches[0].observed
        );
    }
    Ok(0)
}

fn load_records_for_command(
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    source_set: Option<&Path>,
    source_locator: Option<&str>,
    runner: RunnerOutput,
) -> Result<Vec<OperationRecord>, String> {
    match (source_set, source_locator) {
        (None, None) => load_records(flake, nix_override, nix_flags, runner),
        (Some(path), Some(locator)) => {
            let set = read_source_set(path)?;
            let locators = parse_source_locators(locator)?;
            load_records_from_source_set(&set, &locators, nix_override, nix_flags, runner)
        }
        (Some(_), None) => Err("--source-locator is required with --source-set".to_owned()),
        (None, Some(_)) => Err("--source-set is required with --source-locator".to_owned()),
    }
}

pub(crate) fn load_records(
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<Vec<OperationRecord>, String> {
    let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
    let cwd = Utf8Path::from_path(&cwd).ok_or("current directory is not utf-8")?;
    let selection = resolve_flake(flake, cwd).map_err(|err| err.to_string())?;
    let root = selection
        .local_root
        .clone()
        .map(|path| path.into_std_path_buf())
        .unwrap_or_else(|| cwd.as_std_path().to_path_buf());
    // Empty fallback keeps single-checkout revision as git HEAD only.
    let workspace = workspace_instance_from_checkout(&root, "");
    let mut records = Vec::new();
    let mut discover_error = None;
    match build_adapter(nix_override) {
        Ok(adapter) => {
            match adapter.discover_apps(&selection.nix_ref, nix_flags) {
                Ok(apps) => {
                    for app in apps {
                        let mut declaration =
                            Declaration::named(OperationKind::App, app.name, app.attr_path);
                        declaration.description = app.description;
                        records.push(operation_record(&workspace, &declaration));
                    }
                }
                Err(error) => discover_error = Some(error.to_string()),
            }
            match adapter.discover_tasks(&selection.nix_ref, nix_flags) {
                Ok(document) => {
                    for (name, task) in document.tasks {
                        let mut declaration =
                            Declaration::named(OperationKind::Task, name, "flake.nix");
                        declaration.description = task.description;
                        declaration.aliases = task.aliases;
                        declaration.shell = task.shell;
                        declaration.working_directory = task.working_directory;
                        declaration.parameters = task.parameters.keys().cloned().collect();
                        declaration.inputs =
                            task.inputs.map(|inputs| inputs.paths).unwrap_or_default();
                        records.push(operation_record(&workspace, &declaration));
                    }
                    for (name, process) in document.processes {
                        let mut declaration =
                            Declaration::named(OperationKind::Process, name, "flake.nix");
                        declaration.shell = process.shell;
                        declaration.working_directory = process.working_directory;
                        records.push(operation_record(&workspace, &declaration));
                    }
                }
                Err(error) => discover_error = Some(error.to_string()),
            }
        }
        Err(error) => discover_error = Some(error.to_string()),
    }
    let _ = runner;
    if let Some(root) = selection.local_root {
        records.extend(file_records(&workspace, root.as_std_path()));
    }
    records.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
    records.dedup_by(|left, right| left.operation_id == right.operation_id);
    if records.is_empty()
        && let Some(error) = discover_error
    {
        return Err(error);
    }
    Ok(records)
}

/// Discover operations across declared SourceSet member checkouts.
///
/// Each member is located with [`nxr_workspace::discover_member`]. Missing
/// locators and duplicate operation ids fail. Searching does not grant
/// `execution_allowed` or `remote_allowed`.
///
/// # Errors
///
/// Returns an error when the SourceSet is invalid, a member has no locator,
/// a checkout cannot be discovered, or two members share an operation id.
pub(crate) fn load_records_from_source_set(
    source_set: &SourceSetV2,
    locators: &SourceLocators,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<Vec<OperationRecord>, String> {
    let _ = runner;
    nxr_task::identity::source_set_v2(source_set)?;
    if source_set.schema_version != nxr_task::SOURCE_SET_V2 {
        return Err(format!(
            "source set schema `{}` is not supported",
            source_set.schema_version
        ));
    }
    let mut owned_workspaces = Vec::new();
    let mut owned_declarations = Vec::new();
    for member in &source_set.members {
        let checkout = checkout_for_member(source_set, member, locators)?;
        let checkout = Utf8Path::from_path(&checkout)
            .ok_or_else(|| format!("source member `{}` checkout is not utf-8", member.name))?;
        let context = nxr_workspace::discover_member(checkout).map_err(|err| err.to_string())?;
        let root = context.flake_root.as_std_path();
        let workspace = workspace_instance_from_checkout(root, &member.commit);
        let declarations =
            declarations_for_checkout(root, context.flake_ref.as_str(), nix_override, nix_flags);
        owned_workspaces.push((member.name.clone(), workspace));
        owned_declarations.push(declarations);
    }
    let members: Vec<(&str, &WorkspaceInstance, &[Declaration])> = owned_workspaces
        .iter()
        .zip(owned_declarations.iter())
        .map(|((name, workspace), declarations)| {
            (name.as_str(), workspace, declarations.as_slice())
        })
        .collect();
    let mut records = discover_members(&members).map_err(|err| err.to_string())?;
    records.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
    Ok(records)
}

/// Parse the SourceSet locator JSON used by `--source-locator` and MCP `locators`.
///
/// # Errors
///
/// Returns an error when the document is missing `primary` or a sibling path.
pub(crate) fn parse_source_locators(raw: &str) -> Result<SourceLocators, String> {
    parse_source_locators_value(
        &serde_json::from_str(raw).map_err(|err| format!("source locator: {err}"))?,
    )
}

/// Parse a locator object (`primary` plus `siblings`).
///
/// # Errors
///
/// Returns an error when `primary` is missing or a sibling path is empty.
pub(crate) fn parse_source_locators_value(value: &Value) -> Result<SourceLocators, String> {
    let primary = value
        .get("primary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if primary.is_empty() {
        return Err("source locator is missing primary".to_owned());
    }
    let mut siblings = BTreeMap::new();
    if let Some(map) = value.get("siblings").and_then(Value::as_object) {
        for (path, locator) in map {
            let locator = locator.as_str().unwrap_or("").trim();
            if locator.is_empty() {
                return Err(format!("sibling locator for {path} is empty"));
            }
            siblings.insert(path.clone(), PathBuf::from(locator));
        }
    }
    Ok(SourceLocators {
        primary: PathBuf::from(primary),
        siblings,
    })
}

fn read_source_set(path: &Path) -> Result<SourceSetV2, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("source set: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("source set: {err}"))
}

fn checkout_for_member(
    source_set: &SourceSetV2,
    member: &nxr_task::SourceMemberV2,
    locators: &SourceLocators,
) -> Result<PathBuf, String> {
    if member.name == source_set.primary {
        return Ok(locators.primary.clone());
    }
    locators
        .siblings
        .get(&member.logical_path)
        .cloned()
        .ok_or_else(|| format!("source member `{}` has no locator", member.name))
}

fn declarations_for_checkout(
    root: &Path,
    flake_ref: &str,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
) -> Vec<Declaration> {
    let mut declarations = Vec::new();
    if let Ok(adapter) = build_adapter(nix_override) {
        if let Ok(apps) = adapter.discover_apps(flake_ref, nix_flags) {
            for app in apps {
                let mut declaration =
                    Declaration::named(OperationKind::App, app.name, app.attr_path);
                declaration.description = app.description;
                declarations.push(declaration);
            }
        }
        if let Ok(document) = adapter.discover_tasks(flake_ref, nix_flags) {
            for (name, task) in document.tasks {
                let mut declaration = Declaration::named(OperationKind::Task, name, "flake.nix");
                declaration.description = task.description;
                declaration.aliases = task.aliases;
                declaration.shell = task.shell;
                declaration.working_directory = task.working_directory;
                declaration.parameters = task.parameters.keys().cloned().collect();
                declaration.inputs = task.inputs.map(|inputs| inputs.paths).unwrap_or_default();
                declarations.push(declaration);
            }
            for (name, process) in document.processes {
                let mut declaration = Declaration::named(OperationKind::Process, name, "flake.nix");
                declaration.shell = process.shell;
                declaration.working_directory = process.working_directory;
                declarations.push(declaration);
            }
        }
    }
    declarations.extend(file_declarations(root));
    let mut seen = std::collections::BTreeSet::new();
    declarations.retain(|declaration| {
        seen.insert(format!(
            "{}:{}",
            declaration.kind.as_str(),
            declaration.name
        ))
    });
    declarations
}

fn file_records(workspace: &WorkspaceInstance, root: &Path) -> Vec<OperationRecord> {
    file_declarations(root)
        .into_iter()
        .map(|declaration| operation_record(workspace, &declaration))
        .collect()
}

fn file_declarations(root: &Path) -> Vec<Declaration> {
    let mut declarations = Vec::new();
    if let Ok(Some(document)) = nxr_task::load_workflow_document(root) {
        for (name, task) in document.tasks {
            let mut declaration = Declaration::named(
                OperationKind::Workflow,
                name,
                nxr_task::workflows::WORKFLOWS_RELATIVE_PATH,
            );
            declaration.working_directory = task.working_directory;
            declaration.inputs = task.inputs;
            declaration.rustc_wrapper = task.environment.get("RUSTC_WRAPPER").cloned();
            declarations.push(declaration);
        }
    }
    let scripts = root.join(SCRIPT_CONVENTION_DIR);
    if let Ok(entries) = std::fs::read_dir(&scripts) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name().and_then(|file| file.to_str())
            {
                let definition = format!("{SCRIPT_CONVENTION_DIR}/{name}");
                declarations.push(Declaration::named(OperationKind::Script, name, definition));
            }
        }
    }
    declarations
}

fn write_json_or_lines<T: serde::Serialize>(
    json: bool,
    records: &[T],
    line: impl Fn(&T) -> String,
) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(records).map_err(|err| err.to_string())?
        );
    } else {
        let mut stdout = io::stdout().lock();
        for record in records {
            writeln!(stdout, "{}", line(record)).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_options::{ColorWhen, LogFormat, OutputOptions};
    use nxr_task::{SOURCE_SET_V2, SourceMemberV2, execution_decision};
    use std::fs;

    fn quiet_runner() -> RunnerOutput {
        RunnerOutput::new(OutputOptions::new(
            1,
            0,
            true,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ))
    }

    fn member_checkout(root: &Path, workspace_id: &str, task: &str) {
        fs::create_dir_all(root.join(".nxr")).unwrap();
        fs::write(root.join("flake.nix"), "{}\n").unwrap();
        fs::write(root.join(".nxr/workspace-id"), format!("{workspace_id}\n")).unwrap();
        fs::write(
            root.join(".nxr/workflows.toml"),
            format!(
                r#"
schema_version = 1

[tasks.{task}]
executor = {{ native = ["true"] }}
"#
            ),
        )
        .unwrap();
    }

    fn two_member_set(lattice_commit: &str, stargate_commit: &str) -> SourceSetV2 {
        SourceSetV2 {
            schema_version: SOURCE_SET_V2.to_owned(),
            primary: "lattice".to_owned(),
            members: vec![
                SourceMemberV2 {
                    name: "lattice".to_owned(),
                    logical_path: "lattice".to_owned(),
                    kind: "git".to_owned(),
                    commit: lattice_commit.to_owned(),
                    submodules: Vec::new(),
                    lockfiles: Vec::new(),
                    dirty_snapshot_id: None,
                },
                SourceMemberV2 {
                    name: "stargate".to_owned(),
                    logical_path: "deps/stargate".to_owned(),
                    kind: "git".to_owned(),
                    commit: stargate_commit.to_owned(),
                    submodules: Vec::new(),
                    lockfiles: Vec::new(),
                    dirty_snapshot_id: None,
                },
            ],
        }
    }

    #[test]
    fn the_cli_helper_returns_the_library_id() {
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: "/tmp/lattice".to_owned(),
            source_revision: None,
        };
        let record = operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "rust-test-pkgs", "flake.nix"),
        );
        let nixplane = operation_record(
            &WorkspaceInstance {
                workspace_id: "nixplane".to_owned(),
                workspace_root: "/tmp/nixplane".to_owned(),
                source_revision: None,
            },
            &Declaration::named(OperationKind::Task, "test", "flake.nix"),
        );
        let records = vec![record, nixplane];
        assert_eq!(
            matching_ids(&records, "rust-test-pkgs"),
            vec!["lattice:task:rust-test-pkgs".to_owned()]
        );
        assert_eq!(
            describe(&records, "nixplane:task:test").map(|record| record.operation_id.as_str()),
            Some("nixplane:task:test")
        );
    }

    #[test]
    fn source_set_discovery_stamps_member_provenance_without_granting_execution() {
        let lattice = tempfile::tempdir().unwrap();
        let stargate = tempfile::tempdir().unwrap();
        member_checkout(lattice.path(), "lattice", "rust-test-pkgs");
        member_checkout(stargate.path(), "stargate", "test");
        let set = two_member_set("rev-lattice", "rev-stargate");
        let locators = SourceLocators {
            primary: lattice.path().to_path_buf(),
            siblings: BTreeMap::from([("deps/stargate".to_owned(), stargate.path().to_path_buf())]),
        };
        let records = load_records_from_source_set(
            &set,
            &locators,
            Some("/definitely/missing/nix"),
            &OptionalNixFlags::default(),
            quiet_runner(),
        )
        .expect("two members");
        assert_eq!(records.len(), 2);
        let lattice_record = records
            .iter()
            .find(|record| record.name == "rust-test-pkgs")
            .expect("lattice task");
        assert_eq!(lattice_record.source_member.as_deref(), Some("lattice"));
        assert_eq!(
            lattice_record.source_revision.as_deref(),
            Some("rev-lattice")
        );
        let (_, _, allowed) = execution_decision(
            lattice_record.kind,
            &lattice_record.name,
            lattice_record.authorized_effect,
        );
        assert_eq!(lattice_record.execution_allowed, allowed);
        let stargate_record = records
            .iter()
            .find(|record| record.name == "test")
            .expect("stargate task");
        assert_eq!(stargate_record.source_member.as_deref(), Some("stargate"));
        assert_eq!(
            stargate_record.source_revision.as_deref(),
            Some("rev-stargate")
        );
        let (_, _, allowed) = execution_decision(
            stargate_record.kind,
            &stargate_record.name,
            stargate_record.authorized_effect,
        );
        assert_eq!(stargate_record.execution_allowed, allowed);
    }

    #[test]
    fn source_set_duplicate_operation_id_errors() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        member_checkout(left.path(), "shared", "fmt");
        member_checkout(right.path(), "shared", "fmt");
        let set = two_member_set("rev-a", "rev-b");
        let locators = SourceLocators {
            primary: left.path().to_path_buf(),
            siblings: BTreeMap::from([("deps/stargate".to_owned(), right.path().to_path_buf())]),
        };
        let error = load_records_from_source_set(
            &set,
            &locators,
            Some("/definitely/missing/nix"),
            &OptionalNixFlags::default(),
            quiet_runner(),
        )
        .expect_err("duplicate");
        assert!(error.contains("shared:workflow:fmt"), "{error}");
        assert!(
            error.contains("lattice") && error.contains("stargate"),
            "{error}"
        );
    }

    #[test]
    fn missing_member_locator_names_the_member() {
        let lattice = tempfile::tempdir().unwrap();
        member_checkout(lattice.path(), "lattice", "fmt");
        let set = two_member_set("rev-a", "rev-b");
        let locators = SourceLocators {
            primary: lattice.path().to_path_buf(),
            siblings: BTreeMap::new(),
        };
        let error = load_records_from_source_set(
            &set,
            &locators,
            Some("/definitely/missing/nix"),
            &OptionalNixFlags::default(),
            quiet_runner(),
        )
        .expect_err("missing locator");
        assert!(error.contains("stargate"), "{error}");
        assert!(error.contains("no locator"), "{error}");
    }

    #[test]
    fn load_records_without_source_set_leaves_source_member_unset() {
        let root = tempfile::tempdir().unwrap();
        member_checkout(root.path(), "solo", "fmt");
        let records = load_records(
            Some(root.path().to_str().expect("utf-8")),
            Some("/definitely/missing/nix"),
            &OptionalNixFlags::default(),
            quiet_runner(),
        )
        .expect("single checkout");
        assert!(!records.is_empty());
        assert!(records.iter().all(|record| record.source_member.is_none()));
    }
}
