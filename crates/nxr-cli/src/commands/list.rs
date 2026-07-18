//! `nxr list` command implementation.

use std::io;

use nxr_completion::cache::{DiscoveryCacheOptions, DiscoveryContext, discover_with_cache};
use nxr_nix::NixError;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::output::{JsonListError, write_human_list, write_json_list};
use crate::runner_output::RunnerOutput;

/// Errors while running the list command.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Json(#[from] JsonListError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ListError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Json(_) | Self::Io(_) => nxr_core::diagnostics::exit::EVALUATION,
        }
    }
}

/// Discover and print apps for the selected flake.
///
/// # Errors
///
/// Returns [`ListError`] when flake resolution, Nix discovery, or output fails.
pub fn run(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    refresh: bool,
    runner: RunnerOutput,
) -> Result<(), ListError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    runner
        .info(format!("discovering apps for {}", flake.display))
        .map_err(ListError::Io)?;
    let adapter = build_adapter(nix_override)?;
    let apps = discover_apps(&flake, &adapter, refresh)?;
    runner
        .verbose(format!(
            "found {} app(s) for system {}",
            apps.len(),
            adapter.system
        ))
        .map_err(ListError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        write_json_list(&mut stdout, &flake.display, &adapter.system, &apps)?;
    } else {
        write_human_list(&mut stdout, &adapter.system, &apps)?;
    }

    Ok(())
}

fn discover_apps(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh: bool,
) -> Result<Vec<nxr_core::App>, NixError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
    };
    let flake_ref = flake.nix_ref.clone();

    discover_with_cache(&context, DiscoveryCacheOptions { refresh }, || {
        adapter.discover_apps(&flake_ref)
    })
}

#[cfg(test)]
mod tests {
    use crate::commands::common::current_invocation_directory;

    #[test]
    fn invocation_directory_is_valid_utf8_path() {
        let cwd = current_invocation_directory().expect("current directory");
        assert!(cwd.is_absolute() || !cwd.as_str().is_empty());
    }
}
