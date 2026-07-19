//! Glob include/exclude filters for watch path selection.

use std::path::Path;

use camino::Utf8Path;
use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

/// Errors while compiling user-supplied glob patterns.
#[derive(Debug, Error)]
pub enum PathFilterError {
    /// A glob pattern could not be parsed.
    #[error("invalid glob pattern `{pattern}`: {source}")]
    InvalidGlob {
        /// The user-supplied pattern.
        pattern: String,
        /// Underlying globset error.
        source: globset::Error,
    },
}

/// Compiled include/exclude globs applied on top of built-in ignores.
#[derive(Clone, Debug, Default)]
pub struct PathFilters {
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
}

impl PathFilters {
    /// No additional include/exclude globs (built-in ignores only).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Compile optional include and exclude glob lists.
    ///
    /// When `include` is non-empty, only paths matching at least one include
    /// glob can trigger a restart (after built-in ignores).
    ///
    /// # Errors
    ///
    /// Returns [`PathFilterError`] when any pattern is invalid.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, PathFilterError> {
        Ok(Self {
            include: compile_globs(include)?,
            exclude: compile_globs(exclude)?,
            has_include: !include.is_empty(),
        })
    }
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, PathFilterError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|source| PathFilterError::InvalidGlob {
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|source| PathFilterError::InvalidGlob {
            pattern: patterns
                .first()
                .cloned()
                .unwrap_or_else(|| "<empty>".to_owned()),
            source,
        })
}

/// Whether `path` should be ignored relative to the watch root.
#[must_use]
pub fn should_ignore_path(root: &Utf8Path, path: &Path, filters: &PathFilters) -> bool {
    let Some(path) = Utf8Path::from_path(path) else {
        return true;
    };

    if is_builtin_ignored(path) {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = relative_for_glob(relative.as_str());

    if filters.exclude.is_match(relative) {
        return true;
    }

    if filters.has_include && !filters.include.is_match(relative) {
        return true;
    }

    false
}

fn is_builtin_ignored(path: &Utf8Path) -> bool {
    if path.starts_with("/nix/store") {
        return true;
    }

    path.components().any(|component| {
        let name = component.as_str();
        name == ".git" || name == "target" || name == "result" || name.starts_with("result-")
    })
}

fn relative_for_glob(relative: &str) -> &str {
    relative.strip_prefix("./").unwrap_or(relative)
}

/// Test helper: expose ignore logic with owned paths.
#[cfg(test)]
pub(crate) fn ignore_check(root: &str, path: &str, filters: &PathFilters) -> bool {
    should_ignore_path(Utf8Path::new(root), Path::new(path), filters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_builtin_paths_without_filters() {
        let filters = PathFilters::none();
        let root = "/proj";
        assert!(ignore_check(root, "/proj/.git/HEAD", &filters));
        assert!(ignore_check(root, "/proj/target/debug/nxr", &filters));
        assert!(ignore_check(root, "/proj/result", &filters));
        assert!(ignore_check(root, "/proj/result-1", &filters));
        assert!(ignore_check(root, "/nix/store/abc/bin/hello", &filters));
        assert!(!ignore_check(root, "/proj/src/main.rs", &filters));
    }

    #[test]
    fn exclude_glob_ignores_matching_paths() {
        let filters = PathFilters::new(&[], &["docs/**".to_owned()]).expect("filters");
        let root = "/proj";
        assert!(ignore_check(root, "/proj/docs/guide.md", &filters));
        assert!(!ignore_check(root, "/proj/src/main.rs", &filters));
    }

    #[test]
    fn include_glob_restricts_to_matching_paths() {
        let filters = PathFilters::new(&["src/**".to_owned()], &[]).expect("filters");
        let root = "/proj";
        assert!(!ignore_check(root, "/proj/src/main.rs", &filters));
        assert!(ignore_check(root, "/proj/README.md", &filters));
    }

    #[test]
    fn include_and_exclude_combine() {
        let filters = PathFilters::new(&["src/**".to_owned()], &["src/generated/**".to_owned()])
            .expect("filters");
        let root = "/proj";
        assert!(!ignore_check(root, "/proj/src/lib.rs", &filters));
        assert!(ignore_check(root, "/proj/src/generated/out.rs", &filters));
        assert!(ignore_check(root, "/proj/tests/mod.rs", &filters));
    }

    #[test]
    fn invalid_glob_returns_error() {
        let error = PathFilters::new(&["[".to_owned()], &[]).expect_err("invalid");
        assert!(error.to_string().contains("invalid glob pattern"));
    }

    #[test]
    fn relative_for_glob_strips_dot_slash() {
        assert_eq!(relative_for_glob("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn builtin_ignore_checks_path_components() {
        assert!(is_builtin_ignored(Utf8Path::new("/proj/.git/config")));
        assert!(!is_builtin_ignored(Utf8Path::new("/proj/src/main.rs")));
        assert!(is_builtin_ignored(Utf8Path::new("/proj/target/debug/nxr")));
    }
}
