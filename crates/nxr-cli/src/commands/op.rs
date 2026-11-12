//! `nxr op` and `nxr env explain`.
//!
//! Both commands call [`nxr_task::catalog`] and do not run the operation.

use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use camino::Utf8Path;
use nxr_nix::OptionalNixFlags;
use nxr_task::{
    Declaration, OperationKind, OperationRecord, WorkspaceInstance, describe, explain,
    operation_record, plan, search, workspace_id,
};

use crate::cli::{EnvCommand, OpCommand};
use crate::commands::common::build_adapter;
use crate::commands::script::SCRIPT_CONVENTION_DIR;
use crate::flake::resolve_flake;
use crate::runner_output::RunnerOutput;

/// Ids returned for a query. The CLI prints these same ids.
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
    runner: RunnerOutput,
) -> Result<i32, String> {
    let records = load_records(flake, nix_override, nix_flags, runner)?;
    match command {
        OpCommand::Search { query } => {
            let matched = search(&records, query);
            write_json_or_lines(json, &matched, |record| record.operation_id.clone())?;
        }
        OpCommand::Describe { id } => {
            let record = describe(&records, id).ok_or_else(|| format!("unknown operation {id}"))?;
            write_json_or_lines(json, &[record.clone()], |record| {
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
    let directory_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let workspace = WorkspaceInstance {
        workspace_id: workspace_id(
            explicit_workspace_id(&root).as_deref(),
            git_remote(&root).as_deref(),
            directory_name,
        ),
        workspace_root: root.display().to_string(),
        source_revision: git_revision(&root),
    };
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

fn file_records(workspace: &WorkspaceInstance, root: &Path) -> Vec<OperationRecord> {
    let mut records = Vec::new();
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
            records.push(operation_record(workspace, &declaration));
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
                records.push(operation_record(
                    workspace,
                    &Declaration::named(OperationKind::Script, name, definition),
                ));
            }
        }
    }
    records
}

fn explicit_workspace_id(root: &Path) -> Option<String> {
    let path = root.join(".nxr").join("workspace-id");
    let text = std::fs::read_to_string(path).ok()?;
    let id = text.lines().next().unwrap_or("").trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_owned())
    }
}

fn git_remote(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|remote| remote.trim().to_owned())
        .filter(|remote| !remote.is_empty())
}

fn git_revision(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|revision| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
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
}
