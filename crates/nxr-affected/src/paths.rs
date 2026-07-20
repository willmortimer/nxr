//! Path normalization and conservative overlap checks.

use std::path::{Component, Path};

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

/// Errors while compiling declared path-root globs.
#[derive(Debug, Error)]
pub enum PathRootError {
    /// A declared root contains an invalid glob pattern.
    #[error("invalid path glob `{pattern}`: {message}")]
    InvalidGlob {
        /// The invalid pattern.
        pattern: String,
        /// Underlying globset error message.
        message: String,
    },
    /// A declared root is empty, absolute, or escapes via `..`.
    #[error(
        "invalid path root `{path}`: must be nonempty, repository-relative, and must not contain `..`"
    )]
    InvalidRepoPath {
        /// The invalid root value.
        path: String,
    },
}

/// Normalize a repository-relative path for comparisons.
#[must_use]
pub fn normalize_relative_path(path: &str) -> String {
    let trimmed = path.trim();
    let without_dot = trimmed.strip_prefix("./").unwrap_or(trimmed);
    without_dot.replace('\\', "/")
}

/// Whether a changed path invalidates every discovered node (flake/Nix inputs).
#[must_use]
pub fn is_global_invalidation_path(path: &str) -> bool {
    let normalized = normalize_relative_path(path);
    normalized == "flake.nix"
        || normalized == "flake.lock"
        || Path::new(&normalized)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nix"))
}

/// Whether `root` looks like a glob pattern rather than a plain path prefix.
#[must_use]
pub fn looks_like_glob(root: &str) -> bool {
    root.contains(['*', '?', '[', '{'])
}

/// Validate that every root is repository-relative and that glob-looking roots compile.
///
/// # Errors
///
/// Returns [`PathRootError`] for the first root that escapes the repo or fails to compile.
pub fn validate_path_roots(roots: &[String]) -> Result<(), PathRootError> {
    for root in roots {
        let root = normalize_relative_path(root);
        validate_repo_relative_root(&root)?;
        if looks_like_glob(&root) {
            Glob::new(&root).map_err(|error| PathRootError::InvalidGlob {
                pattern: root,
                message: error.to_string(),
            })?;
        }
    }
    Ok(())
}

fn validate_repo_relative_root(root: &str) -> Result<(), PathRootError> {
    if root.is_empty() {
        return Err(PathRootError::InvalidRepoPath {
            path: root.to_owned(),
        });
    }
    let path = Path::new(root);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PathRootError::InvalidRepoPath {
            path: root.to_owned(),
        });
    }
    Ok(())
}

/// Whether `changed` overlaps any declared root prefix or glob (conservative).
///
/// Prefix overlap treats parent-directory edits as affecting child roots.
///
/// # Errors
///
/// Returns [`PathRootError`] when a root contains an invalid glob. Callers must not
/// treat that as "no match".
pub fn path_matches_roots(changed: &str, roots: &[String]) -> Result<bool, PathRootError> {
    if roots.is_empty() {
        return Ok(false);
    }

    validate_path_roots(roots)?;

    let changed = normalize_relative_path(changed);
    for root in roots {
        let root = normalize_relative_path(root);
        if prefix_overlap(&changed, &root) {
            return Ok(true);
        }
    }

    Ok(compile_globset(roots)?.is_match(changed.as_str()))
}

fn prefix_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn compile_globset(roots: &[String]) -> Result<GlobSet, PathRootError> {
    let mut builder = GlobSetBuilder::new();
    let mut has_glob = false;
    for root in roots {
        let root = normalize_relative_path(root);
        if looks_like_glob(&root) {
            has_glob = true;
            let glob = Glob::new(&root).map_err(|error| PathRootError::InvalidGlob {
                pattern: root.clone(),
                message: error.to_string(),
            })?;
            builder.add(glob);
        }
    }
    if !has_glob {
        return Ok(GlobSetBuilder::new()
            .build()
            .expect("empty GlobSetBuilder always builds"));
    }
    builder.build().map_err(|error| PathRootError::InvalidGlob {
        pattern: "*".to_owned(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        is_global_invalidation_path, normalize_relative_path, path_matches_roots,
        validate_path_roots,
    };

    #[test]
    fn normalize_strips_dot_slash() {
        assert_eq!(normalize_relative_path("./nix/apps.nix"), "nix/apps.nix");
    }

    #[test]
    fn global_paths_include_flake_and_nix_files() {
        assert!(is_global_invalidation_path("flake.nix"));
        assert!(is_global_invalidation_path("flake.lock"));
        assert!(is_global_invalidation_path("nix/apps.nix"));
        assert!(!is_global_invalidation_path("src/main.rs"));
    }

    #[test]
    fn prefix_overlap_is_conservative_for_parents() {
        let roots = vec!["crates/api".to_owned()];
        assert!(path_matches_roots("crates/api/src/lib.rs", &roots).expect("valid"));
        assert!(path_matches_roots("crates", &roots).expect("valid"));
        assert!(!path_matches_roots("crates/web/lib.rs", &roots).expect("valid"));
    }

    #[test]
    fn glob_roots_match_nested_files() {
        let roots = vec!["shared/**".to_owned()];
        assert!(path_matches_roots("shared/lib.txt", &roots).expect("valid"));
    }

    #[test]
    fn invalid_glob_errors_instead_of_silent_miss() {
        let roots = vec!["crates/[api".to_owned()];
        assert!(validate_path_roots(&roots).is_err());
        assert!(path_matches_roots("crates/api/lib.rs", &roots).is_err());
    }

    #[test]
    fn rejects_absolute_and_parent_roots() {
        assert!(validate_path_roots(&["/abs".to_owned()]).is_err());
        assert!(validate_path_roots(&["../escape".to_owned()]).is_err());
        assert!(validate_path_roots(&[String::new()]).is_err());
    }
}
