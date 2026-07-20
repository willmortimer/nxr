//! Hidden dynamic completion protocol for shell integrations.

use std::io::{self, Write};

use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_completion::{CompleteTarget, discover_app_candidates, write_app_candidates};
use nxr_core::projects::{ProjectsDocument, load_projects_document};
use nxr_nix::{OptionalNixFlags, OutputTable};
use nxr_task::listable_tasks;

use crate::commands::common::{build_adapter, current_invocation_directory};
use crate::flake::resolve_flake;

/// Errors while serving dynamic completion candidates.
#[derive(Debug, thiserror::Error)]
pub enum CompleteError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Write dynamic completion candidates to stdout without runner diagnostics.
///
/// Discovery, flake resolution, and evaluation failures fall back to an empty
/// candidate list so shell completion never blocks on errors.
///
/// # Errors
///
/// Returns [`CompleteError`] only when stdout cannot be written.
pub fn run(
    target: CompleteTarget,
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<(), CompleteError> {
    match target {
        CompleteTarget::Apps => {
            write_app_completions(flake_arg, nix_override, refresh_discovery, nix_flags)
        }
        CompleteTarget::Tasks => write_lines(&task_completions(
            flake_arg,
            nix_override,
            refresh_discovery,
            nix_flags,
        )),
        CompleteTarget::Packages => write_lines(&output_completions(
            flake_arg,
            nix_override,
            nix_flags,
            OutputTable::Packages,
        )),
        CompleteTarget::Checks => write_lines(&output_completions(
            flake_arg,
            nix_override,
            nix_flags,
            OutputTable::Checks,
        )),
        CompleteTarget::Shells => write_lines(&output_completions(
            flake_arg,
            nix_override,
            nix_flags,
            OutputTable::DevShells,
        )),
        CompleteTarget::Namespaces => write_lines(&namespace_completions(flake_arg)),
        CompleteTarget::Categories => write_lines(&category_completions(
            flake_arg,
            nix_override,
            refresh_discovery,
            nix_flags,
        )),
    }
}

fn write_app_completions(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<(), CompleteError> {
    let Some(apps) = discover_apps(flake_arg, nix_override, refresh_discovery, nix_flags) else {
        flush_empty()?;
        return Ok(());
    };

    let mut stdout = io::stdout().lock();
    write_app_candidates(&apps, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

fn write_lines(lines: &[String]) -> Result<(), CompleteError> {
    let mut stdout = io::stdout().lock();
    for line in lines {
        writeln!(stdout, "{line}")?;
    }
    stdout.flush()?;
    Ok(())
}

fn discover_apps(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Option<Vec<nxr_core::App>> {
    let invocation_cwd = current_invocation_directory().ok()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd).ok()?;
    let adapter = build_adapter(nix_override).ok()?;

    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };
    let flake_ref = flake.nix_ref.clone();
    let nix_flags = nix_flags.clone();

    Some(discover_app_candidates(
        &context,
        DiscoveryCacheOptions {
            refresh: refresh_discovery,
            require_tasks: false,
        },
        move || {
            let apps = adapter
                .discover_apps(&flake_ref, &nix_flags)
                .map_err(|error| error.to_string())?;
            let tasks = adapter.discover_tasks(&flake_ref, &nix_flags).ok();
            Ok::<WorkspaceDiscovery, String>(WorkspaceDiscovery { apps, tasks })
        },
    ))
}

fn task_completions(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Vec<String> {
    let Some(workspace) = discover_workspace(flake_arg, nix_override, refresh_discovery, nix_flags)
    else {
        return Vec::new();
    };
    let Some(doc) = workspace.tasks else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for (name, definition) in listable_tasks(&doc, None) {
        names.push(name.clone());
        for alias in &definition.aliases {
            names.push(alias.clone());
        }
    }
    names.sort();
    names.dedup();
    names
}

fn output_completions(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    table: OutputTable,
) -> Vec<String> {
    let Some((flake, adapter)) = resolve_adapter(flake_arg, nix_override) else {
        return Vec::new();
    };
    let Ok(outputs) = adapter.discover_outputs(&flake.nix_ref, table, nix_flags) else {
        return Vec::new();
    };
    let mut names: Vec<String> = outputs.into_iter().map(|output| output.name).collect();
    names.sort();
    names
}

fn namespace_completions(flake_arg: Option<&str>) -> Vec<String> {
    let Some(root) = local_root(flake_arg) else {
        return Vec::new();
    };
    let Ok(Some((_path, doc))) = load_projects_document(root.as_std_path()) else {
        return Vec::new();
    };
    namespace_names(&doc)
}

fn category_completions(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Vec<String> {
    let Some(workspace) = discover_workspace(flake_arg, nix_override, refresh_discovery, nix_flags)
    else {
        return Vec::new();
    };
    let mut categories = std::collections::BTreeSet::new();
    if let Some(doc) = &workspace.tasks {
        for definition in doc.tasks.values() {
            if let Some(category) = &definition.category {
                categories.insert(category.clone());
            }
        }
        for meta in doc.apps.values() {
            if let Some(category) = &meta.category {
                categories.insert(category.clone());
            }
        }
    }
    categories.into_iter().collect()
}

fn discover_workspace(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Option<WorkspaceDiscovery> {
    let invocation_cwd = current_invocation_directory().ok()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd).ok()?;
    let adapter = build_adapter(nix_override).ok()?;
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };
    let flake_ref = flake.nix_ref.clone();
    let nix_flags = nix_flags.clone();
    discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions {
            refresh: refresh_discovery,
            require_tasks: false,
        },
        move || {
            let apps = adapter
                .discover_apps(&flake_ref, &nix_flags)
                .map_err(|error| error.to_string())?;
            let tasks = adapter.discover_tasks(&flake_ref, &nix_flags).ok();
            Ok::<WorkspaceDiscovery, String>(WorkspaceDiscovery { apps, tasks })
        },
    )
    .ok()
}

fn resolve_adapter(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
) -> Option<(crate::flake::FlakeSelection, nxr_nix::NixAdapter)> {
    let invocation_cwd = current_invocation_directory().ok()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd).ok()?;
    let adapter = build_adapter(nix_override).ok()?;
    Some((flake, adapter))
}

fn local_root(flake_arg: Option<&str>) -> Option<camino::Utf8PathBuf> {
    let invocation_cwd = current_invocation_directory().ok()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd).ok()?;
    flake.local_root
}

fn namespace_names(doc: &ProjectsDocument) -> Vec<String> {
    let mut names: Vec<String> = doc.projects.keys().cloned().collect();
    names.sort();
    names
}

fn flush_empty() -> Result<(), io::Error> {
    io::stdout().lock().flush()
}
