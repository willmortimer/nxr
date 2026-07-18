//! Flake reference resolution for CLI invocations.

use camino::Utf8Path;
use nxr_workspace::{DiscoveryError, WorkspaceContext, discover_from};

/// Selected flake reference for discovery and JSON output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlakeSelection {
    /// User-facing flake string for list JSON (`flake` field).
    pub display: String,
    /// Reference passed to `nix flake show`.
    pub nix_ref: String,
}

/// Errors while resolving which flake to use.
#[derive(Debug, thiserror::Error)]
pub enum FlakeResolveError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
}

impl FlakeResolveError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Discovery(error) => error.exit_code(),
        }
    }
}

/// Resolve the flake to list from an optional `--flake` override.
///
/// Local paths are resolved relative to `invocation_cwd`. Remote flake URIs are
/// passed through unchanged.
///
/// # Errors
///
/// Returns [`FlakeResolveError`] when no `--flake` was given and upward discovery fails.
pub fn resolve_flake(
    flake_arg: Option<&str>,
    invocation_cwd: &Utf8Path,
) -> Result<FlakeSelection, FlakeResolveError> {
    if let Some(reference) = flake_arg {
        Ok(FlakeSelection {
            display: reference.to_owned(),
            nix_ref: resolve_explicit_flake_ref(reference, invocation_cwd),
        })
    } else {
        let context = discover_from(invocation_cwd)?;
        Ok(flake_selection_from_context(&context))
    }
}

fn flake_selection_from_context(context: &WorkspaceContext) -> FlakeSelection {
    let flake = context.flake_ref.as_str().to_owned();
    FlakeSelection {
        display: flake.clone(),
        nix_ref: flake,
    }
}

fn resolve_explicit_flake_ref(reference: &str, invocation_cwd: &Utf8Path) -> String {
    if is_flake_uri(reference) {
        return reference.to_owned();
    }

    let joined = invocation_cwd.join(reference);
    joined.canonicalize_utf8().unwrap_or(joined).into_string()
}

fn is_flake_uri(reference: &str) -> bool {
    let Some(colon) = reference.find(':') else {
        return false;
    };

    if colon == 1 {
        let Some(first) = reference.chars().next() else {
            return false;
        };
        if first.is_ascii_alphabetic() {
            let rest = &reference[2..];
            if rest.starts_with('\\') || rest.starts_with('/') {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use camino::Utf8Path;
    use tempfile::TempDir;

    use super::{is_flake_uri, resolve_explicit_flake_ref, resolve_flake};

    fn utf8_path(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .expect("tempdir path must be valid UTF-8")
    }

    #[test]
    fn remote_refs_are_not_resolved_against_cwd() {
        assert!(is_flake_uri("github:owner/repo"));
        assert_eq!(
            resolve_explicit_flake_ref("github:owner/repo", Utf8Path::new("/tmp")),
            "github:owner/repo"
        );
    }

    #[test]
    fn local_refs_are_resolved_relative_to_invocation_cwd() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let nested = root.join("crates/api");
        fs::create_dir_all(&nested).expect("create nested dir");

        let resolved = resolve_explicit_flake_ref("../..", &nested);
        let expected = root
            .canonicalize_utf8()
            .unwrap_or_else(|_| root.clone());
        let actual = camino::Utf8PathBuf::from(&resolved)
            .canonicalize_utf8()
            .map(|path| path.to_string())
            .unwrap_or(resolved);
        assert_eq!(actual, expected.as_str());
    }

    #[test]
    fn discover_when_flake_flag_omitted() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let selection = resolve_flake(None, &root).expect("discover flake");
        assert_eq!(selection.display, selection.nix_ref);
        assert!(
            selection
                .nix_ref
                .ends_with(temp.path().file_name().unwrap().to_str().unwrap())
        );
    }
}
