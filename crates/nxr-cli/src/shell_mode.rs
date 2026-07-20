//! Dev shell wrap mode and active-shell detection via `NXR_DEV_SHELL`.

use std::env;

use clap::ValueEnum;

/// Environment variable set by shell integration when a dev shell is active.
pub const NXR_DEV_SHELL_ENV: &str = "NXR_DEV_SHELL";

/// Controls whether `nxr --shell <name>` wraps execution in `nix develop`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ShellMode {
    /// Skip `nix develop` when `NXR_DEV_SHELL` matches `--shell` (default).
    #[default]
    Smart,
    /// Always wrap when `--shell` is set, even when the marker matches.
    Always,
    /// Never wrap; `--shell` is ignored.
    Never,
}

/// Read the active dev shell from `NXR_DEV_SHELL` when set and non-empty.
#[must_use]
pub fn active_dev_shell() -> Option<String> {
    env::var(NXR_DEV_SHELL_ENV)
        .ok()
        .filter(|value| !value.is_empty())
}

/// Whether to wrap execution in `nix develop` for the requested shell name.
#[must_use]
pub fn should_wrap_shell(requested: &str, mode: ShellMode) -> bool {
    should_wrap_shell_with_active(requested, mode, active_dev_shell().as_deref())
}

/// Like [`should_wrap_shell`] but accepts an explicit active shell marker.
#[must_use]
pub fn should_wrap_shell_with_active(
    requested: &str,
    mode: ShellMode,
    active: Option<&str>,
) -> bool {
    match mode {
        ShellMode::Never => false,
        ShellMode::Always => true,
        ShellMode::Smart => active != Some(requested),
    }
}

/// Resolve the shell name to pass to `nix develop`, if any.
#[must_use]
pub fn effective_shell_wrap(requested: Option<&str>, mode: ShellMode) -> Option<&str> {
    let name = requested?;
    if should_wrap_shell(name, mode) {
        Some(name)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ShellMode, effective_shell_wrap, should_wrap_shell_with_active,
    };

    #[test]
    fn smart_mode_skips_wrap_when_marker_matches() {
        assert!(!should_wrap_shell_with_active(
            "backend",
            ShellMode::Smart,
            Some("backend")
        ));
    }

    #[test]
    fn smart_mode_wraps_when_marker_differs_or_missing() {
        assert!(should_wrap_shell_with_active(
            "frontend",
            ShellMode::Smart,
            Some("backend")
        ));
        assert!(should_wrap_shell_with_active("backend", ShellMode::Smart, None));
    }

    #[test]
    fn always_and_never_modes_override_marker() {
        assert!(should_wrap_shell_with_active(
            "backend",
            ShellMode::Always,
            Some("backend")
        ));
        assert!(!should_wrap_shell_with_active(
            "backend",
            ShellMode::Never,
            Some("backend")
        ));
        assert_eq!(
            effective_shell_wrap(Some("backend"), ShellMode::Never),
            None
        );
    }
}
