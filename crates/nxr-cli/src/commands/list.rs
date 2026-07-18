//! `nxr list` command implementation.

use std::io;

use nxr_nix::{NixAdapter, NixError, detect_system};

use crate::flake::{FlakeResolveError, resolve_flake};
use crate::output::{JsonListError, write_human_list, write_json_list};

/// Errors while running the list command.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("failed to determine invocation directory: {0}")]
    InvocationDirectory(#[source] io::Error),
    #[error("invocation directory is not valid UTF-8")]
    NonUtf8InvocationDirectory,
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
            Self::InvocationDirectory(_) | Self::NonUtf8InvocationDirectory => {
                nxr_core::diagnostics::exit::DISCOVERY
            }
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
) -> Result<(), ListError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;
    let apps = adapter.discover_apps(&flake.nix_ref)?;

    let mut stdout = io::stdout().lock();
    if json {
        write_json_list(&mut stdout, &flake.display, &adapter.system, &apps)?;
    } else {
        write_human_list(&mut stdout, &adapter.system, &apps)?;
    }

    Ok(())
}

fn current_invocation_directory() -> Result<camino::Utf8PathBuf, ListError> {
    let cwd = std::env::current_dir().map_err(ListError::InvocationDirectory)?;
    camino::Utf8PathBuf::from_path_buf(cwd).map_err(|_| ListError::NonUtf8InvocationDirectory)
}

fn build_adapter(nix_override: Option<&str>) -> Result<NixAdapter, NixError> {
    match nix_override {
        Some(path) => {
            let nix = camino::Utf8PathBuf::from(path);
            if !nix.is_file() {
                return Err(NixError::NixNotFound { path: nix });
            }
            let system = detect_system(&nix)?;
            Ok(NixAdapter::with_nix_and_system(nix, system))
        }
        None => NixAdapter::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::current_invocation_directory;

    #[test]
    fn invocation_directory_is_valid_utf8_path() {
        let cwd = current_invocation_directory().expect("current directory");
        assert!(cwd.is_absolute() || !cwd.as_str().is_empty());
    }
}
