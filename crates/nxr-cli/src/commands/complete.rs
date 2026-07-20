//! Hidden dynamic completion protocol for shell integrations.

use std::io::{self, Write};

use nxr_completion::cache::{DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery};
use nxr_completion::{CompleteTarget, discover_app_candidates, write_app_candidates};

use nxr_nix::OptionalNixFlags;

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

    // Require tasks so apps-only cache entries are misses and cold population
    // records discoveryInputs from the lightweight nxr document.
    Some(discover_app_candidates(
        &context,
        DiscoveryCacheOptions {
            refresh: refresh_discovery,
            require_tasks: true,
        },
        move || {
            let apps = adapter
                .discover_apps(&flake_ref, &nix_flags)
                .map_err(|error| error.to_string())?;
            let tasks = adapter
                .discover_tasks(&flake_ref, &nix_flags)
                .map_err(|error| error.to_string())?;
            Ok::<WorkspaceDiscovery, String>(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
            })
        },
    ))
}

fn flush_empty() -> Result<(), io::Error> {
    io::stdout().lock().flush()
}
