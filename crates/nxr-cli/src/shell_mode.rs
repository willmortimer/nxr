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

/// Resolve shell precedence for task execution: CLI `--shell` > `context.shell` > `task.shell`.
#[must_use]
pub fn resolve_effective_shell(
    cli_shell: Option<&str>,
    context_shell: Option<String>,
    task_shell: Option<String>,
) -> Option<String> {
    cli_shell
        .map(str::to_owned)
        .or(context_shell)
        .or(task_shell)
}

/// Whether `arguments` are a `nix develop … -c <inner…>` wrapper.
#[must_use]
pub fn strip_nix_develop_wrap(arguments: &[String]) -> Option<Vec<String>> {
    if arguments.len() < 4 {
        return None;
    }
    if arguments.first().map(String::as_str) != Some("develop") {
        return None;
    }
    if arguments.get(2).map(String::as_str) != Some("-c") {
        return None;
    }
    Some(arguments[3..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::{
        ShellMode, effective_shell_wrap, resolve_effective_shell, should_wrap_shell_with_active,
        strip_nix_develop_wrap,
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
        assert!(should_wrap_shell_with_active(
            "backend",
            ShellMode::Smart,
            None
        ));
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

    #[test]
    fn resolve_effective_shell_prefers_cli_then_context_then_task() {
        assert_eq!(
            resolve_effective_shell(
                Some("cli"),
                Some("context".to_owned()),
                Some("task".to_owned())
            ),
            Some("cli".to_owned())
        );
        assert_eq!(
            resolve_effective_shell(None, Some("context".to_owned()), Some("task".to_owned())),
            Some("context".to_owned())
        );
        assert_eq!(
            resolve_effective_shell(None, None, Some("task".to_owned())),
            Some("task".to_owned())
        );
    }

    #[test]
    fn strip_nix_develop_wrap_extracts_inner_argv() {
        let wrapped = vec![
            "develop".to_owned(),
            ".#backend".to_owned(),
            "-c".to_owned(),
            "nix".to_owned(),
            "run".to_owned(),
            ".#fmt".to_owned(),
        ];
        assert_eq!(
            strip_nix_develop_wrap(&wrapped),
            Some(vec!["nix".to_owned(), "run".to_owned(), ".#fmt".to_owned()])
        );
        assert_eq!(strip_nix_develop_wrap(&["run".to_owned()]), None);
    }
}
