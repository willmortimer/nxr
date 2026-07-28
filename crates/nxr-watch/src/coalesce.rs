//! Semantic watch change coalescing ([ADR-0161]).
//!
//! Runs after temporal debounce and before metadata/source classification.
//! Temporal debounce remains the backstop; this layer drops spurious paths and
//! collapses bursts that should not trigger extra invalidation work.

use std::collections::{BTreeSet, HashMap};

use camino::{Utf8Path, Utf8PathBuf};
use nxr_affected::normalize_relative_path;

/// Kill-switch for semantic watch coalesce (`off` / `0` / `false` / `no`).
pub const WATCH_COALESCE_ENV: &str = "NXR_WATCH_COALESCE";

/// Minimum same-directory file touches before a formatter-style burst collapses.
const FORMATTER_BURST_THRESHOLD: usize = 3;

/// Outcome of one semantic coalesce pass.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchCoalesceStats {
    /// Input paths removed by coalesce rules.
    pub paths_dropped: u64,
    /// Same-directory bursts collapsed to a directory prefix.
    pub bursts_collapsed: u64,
}

/// Session state for semantic watch coalesce (Wave 5b hook on the snapshot).
#[derive(Clone, Debug, Default)]
pub struct WatchSemanticCoalescer {
    owned_outputs: BTreeSet<String>,
    stats: WatchCoalesceStats,
}

impl WatchSemanticCoalescer {
    /// Whether semantic coalesce is enabled (default on).
    #[must_use]
    pub fn enabled() -> bool {
        Self::coalesce_enabled_from(std::env::var(WATCH_COALESCE_ENV).ok().as_deref())
    }

    fn coalesce_enabled_from(value: Option<&str>) -> bool {
        match value {
            Some(value) => {
                let normalized = value.trim();
                if normalized.is_empty() {
                    return true;
                }
                !matches!(
                    normalized.to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            }
            None => true,
        }
    }

    /// Cumulative coalesce statistics for the watch session.
    #[must_use]
    pub fn stats(&self) -> &WatchCoalesceStats {
        &self.stats
    }

    /// Replace declared workspace outputs for the active generation.
    ///
    /// Changes under these paths are dropped when identifiable (feedback-loop
    /// guard). Call after each task plan load / generation prepare.
    pub fn set_owned_outputs<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.owned_outputs = paths
            .into_iter()
            .map(|path| normalize_relative_path(&path))
            .collect();
    }

    /// Clear owned-output guards (metadata invalidation / session reset).
    pub fn clear_owned_outputs(&mut self) {
        self.owned_outputs.clear();
    }

    /// Apply semantic coalesce rules to a debounced path batch.
    #[must_use]
    pub fn coalesce_paths(&mut self, root: &Utf8Path, paths: Vec<Utf8PathBuf>) -> Vec<Utf8PathBuf> {
        if !Self::enabled() || paths.is_empty() {
            return paths;
        }

        let input_len = paths.len() as u64;
        let relative: Vec<String> = paths
            .iter()
            .filter_map(|path| relative_path(root, path))
            .collect();

        if relative.is_empty() {
            return Vec::new();
        }

        if is_fixture_only_batch(&relative) {
            self.stats.paths_dropped = self.stats.paths_dropped.saturating_add(input_len);
            return Vec::new();
        }

        let mut kept = drop_owned_outputs(&relative, &self.owned_outputs);
        let after_owned = kept.len() as u64;
        self.stats.paths_dropped = self
            .stats
            .paths_dropped
            .saturating_add(input_len.saturating_sub(after_owned));

        kept = drop_editor_temporaries(&kept);
        kept = collapse_create_rename_pairs(&kept);
        kept = collapse_lockfile_batch(&kept);
        let (kept, bursts) = collapse_formatter_bursts(&kept);
        self.stats.bursts_collapsed = self.stats.bursts_collapsed.saturating_add(bursts);

        let final_len = kept.len() as u64;
        self.stats.paths_dropped = self
            .stats
            .paths_dropped
            .saturating_add(after_owned.saturating_sub(final_len));

        kept.into_iter().map(|path| root.join(path)).collect()
    }
}

fn relative_path(root: &Utf8Path, path: &Utf8Path) -> Option<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    Some(normalize_relative_path(relative.as_str()))
}

fn is_fixture_only_batch(paths: &[String]) -> bool {
    !paths.is_empty() && paths.iter().all(|path| is_fixture_relative_path(path))
}

fn is_fixture_relative_path(path: &str) -> bool {
    path == "fixtures"
        || path.starts_with("fixtures/")
        || path == "tests/fixtures"
        || path.starts_with("tests/fixtures/")
}

fn drop_owned_outputs(paths: &[String], owned: &BTreeSet<String>) -> Vec<String> {
    if owned.is_empty() {
        return paths.to_vec();
    }
    paths
        .iter()
        .filter(|path| !is_owned_output(path, owned))
        .cloned()
        .collect()
}

fn is_owned_output(path: &str, owned: &BTreeSet<String>) -> bool {
    owned.iter().any(|output| {
        path == output
            || path.starts_with(&format!("{output}/"))
            || output.starts_with(&format!("{path}/"))
    })
}

fn drop_editor_temporaries(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !is_editor_temporary(path))
        .cloned()
        .collect()
}

fn is_editor_temporary(path: &str) -> bool {
    let Some(name) = path.rsplit('/').next() else {
        return false;
    };
    name.ends_with(".tmp")
        || name.ends_with('~')
        || name.starts_with(".#")
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.contains(".sw")
}

/// Drop temp paths when the batch also contains their likely final rename target.
fn collapse_create_rename_pairs(paths: &[String]) -> Vec<String> {
    if paths.len() < 2 {
        return paths.to_vec();
    }

    let finals: BTreeSet<String> = paths
        .iter()
        .filter(|path| !is_editor_temporary(path))
        .cloned()
        .collect();

    paths
        .iter()
        .filter(|path| {
            if !is_editor_temporary(path) {
                return true;
            }
            let Some(stem) = editor_temp_stem(path) else {
                return true;
            };
            !finals.iter().any(|final_path| {
                final_path == &stem
                    || final_path.ends_with(&format!("/{stem}"))
                    || path_stem(final_path).is_some_and(|s| s == stem)
            })
        })
        .cloned()
        .collect()
}

fn editor_temp_stem(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
    let stem = if let Some(rest) = name.strip_prefix(".#") {
        rest.strip_suffix('.').unwrap_or(rest)
    } else if let Some(base) = name.strip_suffix(".tmp") {
        base
    } else if let Some(base) = name.strip_suffix('~') {
        base
    } else {
        return None;
    };
    if parent.is_empty() {
        Some(stem.to_owned())
    } else {
        Some(format!("{parent}/{stem}"))
    }
}

fn path_stem(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next()?;
    file.rsplit_once('.')
        .map_or(Some(file), |(stem, _)| Some(stem))
}

/// `flake.lock` forces metadata invalidation — sibling paths add no signal.
fn collapse_lockfile_batch(paths: &[String]) -> Vec<String> {
    if paths.iter().any(|path| path == "flake.lock") {
        return vec!["flake.lock".to_owned()];
    }
    paths.to_vec()
}

/// Collapse formatter storms: many sibling files → one directory prefix.
fn collapse_formatter_bursts(paths: &[String]) -> (Vec<String>, u64) {
    let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        let parent = path
            .rsplit_once('/')
            .map_or_else(String::new, |(parent, _)| parent.to_owned());
        by_parent.entry(parent).or_default().push(path.clone());
    }

    let mut collapsed = 0u64;
    let mut out = Vec::new();
    for (parent, group) in by_parent {
        if group.len() >= FORMATTER_BURST_THRESHOLD {
            collapsed = collapsed.saturating_add(1);
            if parent.is_empty() {
                out.extend(group);
            } else {
                out.push(parent);
            }
        } else {
            out.extend(group);
        }
    }
    out.sort();
    out.dedup();
    (out, collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coalesce(root: &str, paths: &[&str]) -> Vec<String> {
        let mut coalescer = WatchSemanticCoalescer::default();
        let root = Utf8Path::new(root);
        let paths = paths.iter().map(|path| root.join(path)).collect::<Vec<_>>();
        coalescer
            .coalesce_paths(root, paths)
            .into_iter()
            .map(|path| {
                path.strip_prefix(root)
                    .map_or_else(|_| path.to_string(), |p| p.to_string())
            })
            .collect()
    }

    #[test]
    fn create_rename_pair_keeps_final_path() {
        let out = coalesce("/proj", &["src/main.rs.tmp", "src/main.rs"]);
        assert_eq!(out, vec!["src/main.rs"]);
    }

    #[test]
    fn formatter_burst_collapses_same_directory() {
        let out = coalesce(
            "/proj",
            &["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "other/x.rs"],
        );
        assert_eq!(out, vec!["other/x.rs", "src"]);
    }

    #[test]
    fn lockfile_batch_keeps_lock_only() {
        let out = coalesce("/proj", &["flake.lock", "src/lib.rs", "Cargo.toml"]);
        assert_eq!(out, vec!["flake.lock"]);
    }

    #[test]
    fn lockfile_classifies_as_metadata_not_broadened() {
        use nxr_affected::is_global_invalidation_path;
        let path = "flake.lock";
        assert!(is_global_invalidation_path(path));
    }

    #[test]
    fn kill_switch_values() {
        assert!(WatchSemanticCoalescer::coalesce_enabled_from(None));
        assert!(WatchSemanticCoalescer::coalesce_enabled_from(Some("")));
        assert!(!WatchSemanticCoalescer::coalesce_enabled_from(Some("off")));
        assert!(!WatchSemanticCoalescer::coalesce_enabled_from(Some("0")));
    }

    #[test]
    fn owned_outputs_are_dropped() {
        let mut coalescer = WatchSemanticCoalescer::default();
        coalescer.set_owned_outputs(["target/generated/out.txt".to_owned()]);
        let root = Utf8Path::new("/proj");
        let out = coalescer.coalesce_paths(
            root,
            vec![
                root.join("target/generated/out.txt"),
                root.join("src/main.rs"),
            ],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], root.join("src/main.rs"));
    }

    #[test]
    fn fixture_only_batch_is_empty() {
        let out = coalesce(
            "/proj",
            &[
                "fixtures/basic-apps/flake.nix",
                "fixtures/task-dag/nxr.json",
            ],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn mixed_fixture_and_source_keeps_both() {
        let out = coalesce("/proj", &["fixtures/a.txt", "src/main.rs"]);
        assert_eq!(out, vec!["fixtures/a.txt", "src/main.rs"]);
    }
}
