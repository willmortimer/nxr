//! Ratatui DAG watch and `nxr attach` replay.

pub mod browser;
mod draw;
mod runtime;
mod session;
mod sink;
mod state;

pub use runtime::run_attach_replay;
pub use session::{AttachRunStatus, AttachSessionError, resolve_attach_run};
pub use sink::TuiEventSink;

use std::io::{self, IsTerminal, Write};

use crate::commands::task_params::mux_session_active;
use crate::output_task::TaskOutputMode;

/// Environment kill-switch / force for `--output tui`.
///
/// - unset / empty: default (TTY required; mux falls back to `live`)
/// - `off` / `0` / `false` / `no`: always fall back to `live`
/// - `force` / `on` / `1` / `true` / `yes`: keep TUI even under tmux/zellij
const NXR_TUI_ENV: &str = "NXR_TUI";

/// Resolve `--output tui`, falling back to live when the TUI cannot open safely.
#[must_use]
pub fn resolved_output_mode(requested: Option<TaskOutputMode>) -> Option<TaskOutputMode> {
    match requested {
        Some(TaskOutputMode::Tui) if should_fallback_from_tui() => Some(TaskOutputMode::Live),
        other => other,
    }
}

/// Like [`resolved_output_mode`] but emits the stderr notice when falling back.
#[must_use]
pub fn resolve_task_output_mode(
    requested: Option<TaskOutputMode>,
    stderr: &mut dyn Write,
) -> Option<TaskOutputMode> {
    if matches!(requested, Some(TaskOutputMode::Tui)) && should_fallback_from_tui() {
        let reason = tui_fallback_reason();
        let _ = writeln!(
            stderr,
            "nxr: --output tui {reason}; using live output (set NXR_TUI=force to override mux)"
        );
    }
    resolved_output_mode(requested)
}

fn should_fallback_from_tui() -> bool {
    match tui_env_mode() {
        TuiEnvMode::Off => true,
        TuiEnvMode::Force => !io::stderr().is_terminal(),
        TuiEnvMode::Auto => !io::stderr().is_terminal() || mux_session_active(),
    }
}

fn tui_fallback_reason() -> &'static str {
    match tui_env_mode() {
        TuiEnvMode::Off => "disabled via NXR_TUI",
        TuiEnvMode::Force | TuiEnvMode::Auto if !io::stderr().is_terminal() => {
            "requires an interactive terminal"
        }
        TuiEnvMode::Auto if mux_session_active() => {
            "falls back under tmux/zellij (alternate-screen conflict)"
        }
        _ => "unavailable",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiEnvMode {
    Auto,
    Off,
    Force,
}

fn tui_env_mode() -> TuiEnvMode {
    match std::env::var(NXR_TUI_ENV) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" => TuiEnvMode::Auto,
            "off" | "0" | "false" | "no" => TuiEnvMode::Off,
            "force" | "on" | "1" | "true" | "yes" => TuiEnvMode::Force,
            _ => TuiEnvMode::Auto,
        },
        Err(_) => TuiEnvMode::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn non_tty_stderr_falls_back_to_live() {
        // Cursor is never a terminal; unit tests run with non-TTY stderr.
        let mut stderr = Cursor::new(Vec::new());
        let resolved = resolve_task_output_mode(Some(TaskOutputMode::Tui), &mut stderr);
        assert_eq!(resolved, Some(TaskOutputMode::Live));
        let rendered = String::from_utf8(stderr.into_inner()).expect("utf-8");
        assert!(rendered.contains("using live"));
    }
}
