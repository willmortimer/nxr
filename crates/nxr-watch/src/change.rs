//! Filesystem change classification for watch restarts.

use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_affected::{is_global_invalidation_path, normalize_relative_path};
use nxr_core::projects::PROJECTS_FILENAME;

use crate::filter::{PathFilters, should_ignore_path};

/// How a watch restart should treat workspace discovery state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeClass {
    /// Discovery metadata inputs changed — invalidate snapshot and rediscover.
    Metadata,
    /// Ordinary source change — rerun with the prepared plan.
    Source,
    /// Built-in / user filters — no restart.
    Ignored,
}

/// Flake-root-relative metadata inputs that force snapshot invalidation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataInputRegistry {
    discovery_inputs: Vec<String>,
}

impl MetadataInputRegistry {
    /// Registry with only built-in metadata paths (`flake.nix`, `flake.lock`, …).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace declared `discoveryInputs` after the first tasks snapshot loads.
    pub fn set_discovery_inputs(&mut self, inputs: Vec<String>) {
        self.discovery_inputs = inputs;
    }

    /// Classify a flake-root-relative path.
    #[must_use]
    pub fn classify_relative_path(&self, relative: &str) -> ChangeClass {
        let normalized = normalize_relative_path(relative);
        if is_metadata_relative_path(&normalized, &self.discovery_inputs) {
            ChangeClass::Metadata
        } else {
            ChangeClass::Source
        }
    }
}

/// Classify one absolute path relative to the watch root and filters.
#[must_use]
pub fn classify_watch_path(
    root: &Utf8Path,
    path: &Path,
    filters: &PathFilters,
    registry: &MetadataInputRegistry,
) -> ChangeClass {
    if should_ignore_path(root, path, filters) {
        return ChangeClass::Ignored;
    }

    let Some(relative) = relative_path(root, path) else {
        return ChangeClass::Ignored;
    };

    registry.classify_relative_path(&relative)
}

/// Merge several changed paths into the strongest restart class.
///
/// `Metadata` wins over `Source`; `Ignored` paths are dropped unless every path
/// is ignored.
#[must_use]
pub fn merge_change_classes(classes: impl IntoIterator<Item = ChangeClass>) -> Option<ChangeClass> {
    let mut saw_source = false;
    for class in classes {
        match class {
            ChangeClass::Metadata => return Some(ChangeClass::Metadata),
            ChangeClass::Source => saw_source = true,
            ChangeClass::Ignored => {}
        }
    }
    saw_source.then_some(ChangeClass::Source)
}

/// Classify pending watch paths and return the merged class plus per-path labels.
#[must_use]
pub fn classify_pending_changes(
    root: &Utf8Path,
    paths: &[Utf8PathBuf],
    filters: &PathFilters,
    registry: &MetadataInputRegistry,
) -> Option<(ChangeClass, Vec<(Utf8PathBuf, ChangeClass)>)> {
    if paths.is_empty() {
        return None;
    }

    let labeled = paths
        .iter()
        .map(|path| {
            let class = classify_watch_path(root, path.as_std_path(), filters, registry);
            (path.clone(), class)
        })
        .collect::<Vec<_>>();

    let merged = merge_change_classes(labeled.iter().map(|(_, class)| *class))?;
    Some((merged, labeled))
}

fn is_metadata_relative_path(normalized: &str, discovery_inputs: &[String]) -> bool {
    if is_global_invalidation_path(normalized) || normalized == PROJECTS_FILENAME {
        return true;
    }

    discovery_inputs.iter().any(|input| {
        let input = normalize_relative_path(input);
        normalized == input || normalized.starts_with(&format!("{input}/"))
    })
}

fn relative_path(root: &Utf8Path, path: &Path) -> Option<String> {
    let path = Utf8Path::from_path(path)?;
    let relative = path.strip_prefix(root).unwrap_or(path);
    Some(normalize_relative_path(relative.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn metadata_paths_include_flake_nix_lock_and_discovery_inputs() {
        let registry = MetadataInputRegistry::new();
        assert_eq!(
            registry.classify_relative_path("flake.nix"),
            ChangeClass::Metadata
        );
        assert_eq!(
            registry.classify_relative_path("flake.lock"),
            ChangeClass::Metadata
        );
        assert_eq!(
            registry.classify_relative_path("nix/apps.nix"),
            ChangeClass::Metadata
        );
        assert_eq!(
            registry.classify_relative_path(PROJECTS_FILENAME),
            ChangeClass::Metadata
        );

        let mut with_inputs = MetadataInputRegistry::new();
        with_inputs.set_discovery_inputs(vec!["vendor".to_owned()]);
        assert_eq!(
            with_inputs.classify_relative_path("vendor/manifest.txt"),
            ChangeClass::Metadata
        );
        assert_eq!(
            with_inputs.classify_relative_path("vendor/nested/file.txt"),
            ChangeClass::Metadata
        );
        assert_eq!(
            with_inputs.classify_relative_path("src/main.rs"),
            ChangeClass::Source
        );
    }

    #[test]
    fn merge_prefers_metadata_over_source() {
        assert_eq!(
            merge_change_classes([ChangeClass::Source, ChangeClass::Metadata]),
            Some(ChangeClass::Metadata)
        );
        assert_eq!(
            merge_change_classes([ChangeClass::Ignored, ChangeClass::Source]),
            Some(ChangeClass::Source)
        );
        assert_eq!(merge_change_classes([ChangeClass::Ignored]), None);
    }

    #[test]
    fn classify_pending_skips_builtin_ignored_paths() {
        let root = Utf8Path::new("/proj");
        let filters = PathFilters::none();
        let registry = MetadataInputRegistry::new();
        let paths = vec![Utf8PathBuf::from("/proj/target/debug/nxr")];
        assert!(classify_pending_changes(root, &paths, &filters, &registry).is_none());
        assert!(merge_change_classes([ChangeClass::Ignored]).is_none());
    }

    #[test]
    fn classify_watch_path_marks_source_files() {
        let root = Utf8Path::new("/proj");
        let filters = PathFilters::none();
        let registry = MetadataInputRegistry::new();
        assert_eq!(
            classify_watch_path(root, Path::new("/proj/src/main.rs"), &filters, &registry),
            ChangeClass::Source
        );
    }
}
