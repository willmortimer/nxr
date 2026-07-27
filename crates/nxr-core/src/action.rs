//! Two-tier action classification ([ADR-0147](https://github.com/nxr-dev/nxr/blob/main/docs/adr/0147-two-tier-actions.md)).

/// Execution tier for a task node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionTier {
    /// Runs a flake app via Nix; NXR never caches store paths.
    DerivationBacked,
    /// Declares workspace `outputs` for mutable checkout artifacts.
    WorkspaceAction,
}

/// Whether schema v2 cache policy enables local NXR CAS restore/save.
#[must_use]
pub fn cache_mode_enabled(mode: Option<&str>) -> bool {
    matches!(mode, Some("local" | "shared-read" | "shared"))
}

/// Classify a task from its declared outputs.
///
/// Workspace actions require non-empty `outputs`. Derivation-backed tasks run
/// flake apps only and never write NXR CAS entries.
#[must_use]
pub fn classify_action_tier(outputs_len: usize) -> ActionTier {
    if outputs_len > 0 {
        ActionTier::WorkspaceAction
    } else {
        ActionTier::DerivationBacked
    }
}

/// Whether NXR may restore/save workspace outputs for this task.
#[must_use]
pub fn workspace_cache_enabled(outputs_len: usize, cache_mode: Option<&str>) -> bool {
    outputs_len > 0 && cache_mode_enabled(cache_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_backed_without_outputs() {
        assert_eq!(classify_action_tier(0), ActionTier::DerivationBacked);
        assert!(!workspace_cache_enabled(0, Some("local")));
    }

    #[test]
    fn workspace_action_with_outputs() {
        assert_eq!(classify_action_tier(1), ActionTier::WorkspaceAction);
        assert!(!workspace_cache_enabled(1, None));
        assert!(workspace_cache_enabled(1, Some("local")));
        assert!(!workspace_cache_enabled(1, Some("disabled")));
    }
}
