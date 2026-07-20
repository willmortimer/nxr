//! Repository-relative path validation for metadata and CLI inputs.

use std::path::{Component, Path};

use thiserror::Error;

/// Errors while validating a repository-relative path.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RepoPathError {
    /// Empty path string.
    #[error("{context}: path must not be empty")]
    Empty {
        /// Human label for the field (e.g. `discoveryInputs[0]`).
        context: String,
    },
    /// Absolute path rejected.
    #[error("{context}: path must be repository-relative (got `{value}`)")]
    Absolute {
        /// Human label for the field.
        context: String,
        /// Offending value.
        value: String,
    },
    /// Parent-directory traversal rejected.
    #[error("{context}: path must not contain `..` (got `{value}`)")]
    ParentTraversal {
        /// Human label for the field.
        context: String,
        /// Offending value.
        value: String,
    },
}

/// Validate a nonempty UTF-8 repository-relative path (no absolute / `..`).
///
/// Does not require the path to exist. Callers that read files should still
/// confirm canonical containment under the flake root when resolving.
///
/// # Errors
///
/// Returns [`RepoPathError`] when the value is empty, absolute, or contains `..`.
pub fn validate_repo_relative_path(context: &str, value: &str) -> Result<(), RepoPathError> {
    if value.is_empty() {
        return Err(RepoPathError::Empty {
            context: context.to_owned(),
        });
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(RepoPathError::Absolute {
            context: context.to_owned(),
            value: value.to_owned(),
        });
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RepoPathError::ParentTraversal {
            context: context.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Normalize separators by stripping a leading `./` when present.
#[must_use]
pub fn normalize_repo_relative_path(value: &str) -> &str {
    value.strip_prefix("./").unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::{normalize_repo_relative_path, validate_repo_relative_path};

    #[test]
    fn accepts_nested_relative_paths() {
        validate_repo_relative_path("paths", "crates/api/src").expect("ok");
        validate_repo_relative_path("paths", "./Cargo.toml").expect("ok");
    }

    #[test]
    fn rejects_empty_absolute_and_parent() {
        assert!(validate_repo_relative_path("x", "").is_err());
        assert!(validate_repo_relative_path("x", "/abs").is_err());
        assert!(validate_repo_relative_path("x", "../escape").is_err());
        assert!(validate_repo_relative_path("x", "a/../b").is_err());
    }

    #[test]
    fn normalize_strips_dot_slash() {
        assert_eq!(normalize_repo_relative_path("./foo"), "foo");
        assert_eq!(normalize_repo_relative_path("foo"), "foo");
    }
}
