//! Flake reference resolution for CLI invocations.

use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::FlakeRef;
use nxr_workspace::{DiscoveryError, WorkspaceContext, discover_from};

/// Selected flake reference for discovery and JSON output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlakeSelection {
    /// User-facing flake string for list JSON (`flake` field).
    pub display: String,
    /// Reference passed to `nix flake show` / `nix run`.
    pub nix_ref: String,
    /// Absolute local flake root when the selection is a local path.
    pub local_root: Option<Utf8PathBuf>,
}

/// Parsed inline `flake#app` reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlakeAppRef<'a> {
    pub flake_ref: &'a str,
    pub app: &'a str,
}

/// Errors while parsing an inline `flake#app` reference.
#[derive(Debug, thiserror::Error)]
pub enum ParseFlakeAppRefError {
    #[error("missing app name after '#' in flake reference")]
    EmptyApp,
}

/// Split a token on the first `#`, matching `nix run` installable syntax.
///
/// Returns `Ok(None)` when the token contains no `#`.
///
/// # Errors
///
/// Returns [`ParseFlakeAppRefError`] when `#` is present but the app name is empty.
pub fn parse_flake_app_ref(token: &str) -> Result<Option<FlakeAppRef<'_>>, ParseFlakeAppRefError> {
    let Some((flake_ref, app)) = token.split_once('#') else {
        return Ok(None);
    };

    if app.is_empty() {
        return Err(ParseFlakeAppRefError::EmptyApp);
    }

    Ok(Some(FlakeAppRef { flake_ref, app }))
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

/// Resolve the flake to use from an optional `--flake` override.
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
        Ok(resolve_explicit_selection(reference, invocation_cwd))
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
        local_root: Some(context.flake_root.clone()),
    }
}

fn resolve_explicit_selection(reference: &str, invocation_cwd: &Utf8Path) -> FlakeSelection {
    if is_flake_uri(reference) {
        return FlakeSelection {
            display: reference.to_owned(),
            nix_ref: reference.to_owned(),
            local_root: None,
        };
    }

    let absolute = resolve_explicit_flake_path(reference, invocation_cwd);
    FlakeSelection {
        display: reference.to_owned(),
        nix_ref: FlakeRef::local_path(absolute.as_str()).into_string(),
        local_root: Some(absolute),
    }
}

fn resolve_explicit_flake_path(reference: &str, invocation_cwd: &Utf8Path) -> Utf8PathBuf {
    let joined = invocation_cwd.join(reference);
    joined.canonicalize_utf8().unwrap_or(joined)
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

    use super::{
        ParseFlakeAppRefError, is_flake_uri, parse_flake_app_ref, resolve_explicit_flake_path,
        resolve_explicit_selection, resolve_flake,
    };

    fn utf8_path(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .expect("tempdir path must be valid UTF-8")
    }

    #[test]
    fn remote_refs_are_not_resolved_against_cwd() {
        assert!(is_flake_uri("github:owner/repo"));
        let selection = resolve_explicit_selection("github:owner/repo", Utf8Path::new("/tmp"));
        assert_eq!(selection.nix_ref, "github:owner/repo");
        assert!(selection.local_root.is_none());
    }

    #[test]
    fn local_refs_are_resolved_relative_to_invocation_cwd() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let nested = root.join("crates/api");
        fs::create_dir_all(&nested).expect("create nested dir");

        let resolved = resolve_explicit_flake_path("../..", &nested);
        let expected = root.canonicalize_utf8().unwrap_or_else(|_| root.clone());
        let actual = resolved
            .canonicalize_utf8()
            .unwrap_or_else(|_| resolved.clone());
        assert_eq!(actual, expected);
    }

    #[test]
    fn local_refs_use_path_uri_for_nix() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let selection = resolve_explicit_selection(".", &root);
        let expected_root = root.canonicalize_utf8().unwrap_or(root);
        assert_eq!(
            selection.local_root.as_deref(),
            Some(expected_root.as_path())
        );
        assert_eq!(selection.nix_ref, format!("path:{expected_root}"));
        assert_eq!(selection.display, ".");
    }

    #[test]
    fn discover_when_flake_flag_omitted() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_path(&temp);
        fs::write(root.join("flake.nix"), "{}").expect("write flake.nix");

        let selection = resolve_flake(None, &root).expect("discover flake");
        assert!(selection.local_root.is_some());
        assert!(selection.nix_ref.starts_with("path:"));
        assert!(
            selection
                .nix_ref
                .ends_with(temp.path().file_name().unwrap().to_str().unwrap())
        );
    }

    #[test]
    fn remote_explicit_flake_has_no_local_root() {
        let selection = resolve_explicit_selection("github:owner/repo", Utf8Path::new("/tmp"));
        assert!(selection.local_root.is_none());
        assert_eq!(selection.nix_ref, "github:owner/repo");
    }

    #[test]
    fn parse_flake_app_ref_splits_on_first_hash() {
        assert_eq!(
            parse_flake_app_ref("github:owner/repo#test").expect("parse"),
            Some(super::FlakeAppRef {
                flake_ref: "github:owner/repo",
                app: "test",
            })
        );
        assert_eq!(
            parse_flake_app_ref("./fixtures/basic-apps#hello").expect("parse"),
            Some(super::FlakeAppRef {
                flake_ref: "./fixtures/basic-apps",
                app: "hello",
            })
        );
        assert_eq!(
            parse_flake_app_ref(".#hello").expect("parse"),
            Some(super::FlakeAppRef {
                flake_ref: ".",
                app: "hello",
            })
        );
    }

    #[test]
    fn parse_flake_app_ref_without_hash_returns_none() {
        assert_eq!(parse_flake_app_ref("hello").expect("parse"), None);
    }

    #[test]
    fn parse_flake_app_ref_rejects_empty_app() {
        let error = parse_flake_app_ref("./fixtures/basic-apps#")
            .expect_err("empty app")
            .to_string();
        assert!(matches!(
            parse_flake_app_ref("./fixtures/basic-apps#"),
            Err(ParseFlakeAppRefError::EmptyApp)
        ));
        assert!(error.contains("missing app name"));
    }
}
