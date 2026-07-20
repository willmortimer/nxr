//! Path normalization and conservative overlap checks.

use globset::{Glob, GlobSet, GlobSetBuilder};

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
    normalized == "flake.nix" || normalized == "flake.lock" || normalized.ends_with(".nix")
}

/// Whether `changed` overlaps any declared root prefix or glob (conservative).
///
/// Prefix overlap treats parent-directory edits as affecting child roots.
#[must_use]
pub fn path_matches_roots(changed: &str, roots: &[String]) -> bool {
    if roots.is_empty() {
        return false;
    }

    let changed = normalize_relative_path(changed);
    for root in roots {
        let root = normalize_relative_path(root);
        if prefix_overlap(&changed, &root) {
            return true;
        }
    }

    compile_globset(roots)
        .map(|set| set.is_match(changed.as_str()))
        .unwrap_or(false)
}

fn prefix_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn compile_globset(roots: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut has_glob = false;
    for root in roots {
        if root.contains(['*', '?', '[', '{']) {
            has_glob = true;
            let glob = Glob::new(root).ok()?;
            builder.add(glob);
        }
    }
    if !has_glob {
        return None;
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::{is_global_invalidation_path, normalize_relative_path, path_matches_roots};

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
        assert!(path_matches_roots("crates/api/src/lib.rs", &roots));
        assert!(path_matches_roots("crates", &roots));
        assert!(!path_matches_roots("crates/web/lib.rs", &roots));
    }

    #[test]
    fn glob_roots_match_nested_files() {
        let roots = vec!["shared/**".to_owned()];
        assert!(path_matches_roots("shared/lib.txt", &roots));
    }
}
