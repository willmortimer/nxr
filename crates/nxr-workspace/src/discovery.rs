//! Upward `flake.nix` discovery from an invocation directory.

use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::FlakeRef;
use nxr_core::diagnostics::exit;

use crate::paths;

/// Repository context discovered from the invocation directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceContext {
    /// Directory from which `nxr` was invoked.
    pub invocation_directory: Utf8PathBuf,
    /// Nearest ancestor directory containing `flake.nix`.
    pub flake_root: Utf8PathBuf,
    /// Local flake reference for the discovered root (absolute path).
    pub flake_ref: FlakeRef,
}

/// Errors while discovering a flake root upward from the invocation directory.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// No `flake.nix` was found while walking upward from the start directory.
    #[error("no flake.nix found starting from {start}")]
    FlakeNotFound { start: Utf8PathBuf },
}

impl DiscoveryError {
    /// Stable `nxr` exit code for discovery failures.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        exit::DISCOVERY
    }
}

/// Discover the nearest flake root by walking upward from `cwd`.
///
/// Records the invocation directory and returns an absolute local flake reference.
///
/// # Errors
///
/// Returns [`DiscoveryError::FlakeNotFound`] when no ancestor directory contains
/// `flake.nix`.
pub fn discover_from(cwd: &Utf8Path) -> Result<WorkspaceContext, DiscoveryError> {
    let invocation_directory = absolute_utf8_path(cwd);
    let mut current = invocation_directory.clone();

    loop {
        if paths::has_flake_nix(&current) {
            let flake_root = absolute_utf8_path(&current);
            let flake_ref = FlakeRef::local_path(flake_root.as_str());
            return Ok(WorkspaceContext {
                invocation_directory,
                flake_root,
                flake_ref,
            });
        }

        let Some(parent) = current.parent() else {
            return Err(DiscoveryError::FlakeNotFound {
                start: invocation_directory,
            });
        };
        current = parent.to_path_buf();
    }
}

fn absolute_utf8_path(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{DiscoveryError, discover_from};
    use nxr_core::diagnostics::exit;

    fn utf8_path(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .expect("tempdir path must be valid UTF-8")
    }

    fn canonicalize(path: &camino::Utf8Path) -> camino::Utf8PathBuf {
        path.canonicalize_utf8()
            .unwrap_or_else(|_| path.to_path_buf())
    }

    #[test]
    fn discover_from_flake_root() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let context = discover_from(&root).expect("discover from flake root");

        assert_eq!(context.invocation_directory, canonicalize(&root));
        assert_eq!(context.flake_root, canonicalize(&root));
        assert_eq!(
            context.flake_ref.as_str(),
            format!("path:{}", canonicalize(&root))
        );
    }

    #[test]
    fn discover_from_nested_directory() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let nested = root.join("crates/api");
        fs::create_dir_all(&nested).expect("create nested dir");

        let context = discover_from(&nested).expect("discover from nested dir");

        assert_eq!(context.invocation_directory, canonicalize(&nested));
        assert_eq!(context.flake_root, canonicalize(&root));
        assert_eq!(
            context.flake_ref.as_str(),
            format!("path:{}", canonicalize(&root))
        );
    }

    #[test]
    fn discover_from_missing_flake_maps_to_discovery_exit_code() {
        let temp = TempDir::new().expect("tempdir");
        let start = utf8_path(&temp);
        let nested = start.join("src");
        fs::create_dir_all(&nested).expect("create nested dir");

        let error = discover_from(&nested).expect_err("missing flake should fail");

        assert!(matches!(error, DiscoveryError::FlakeNotFound { .. }));
        assert_eq!(error.exit_code(), exit::DISCOVERY);
    }
}
