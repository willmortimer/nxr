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

use crate::output_task::TaskOutputMode;

/// Resolve `--output tui`, falling back to live output on non-TTY stderr.
#[must_use]
pub fn resolved_output_mode(requested: Option<TaskOutputMode>) -> Option<TaskOutputMode> {
    match requested {
        Some(TaskOutputMode::Tui) if !io::stderr().is_terminal() => Some(TaskOutputMode::Live),
        other => other,
    }
}

/// Like [`resolved_output_mode`] but emits the stderr notice when falling back.
#[must_use]
pub fn resolve_task_output_mode(
    requested: Option<TaskOutputMode>,
    stderr: &mut dyn Write,
) -> Option<TaskOutputMode> {
    if matches!(requested, Some(TaskOutputMode::Tui)) && !io::stderr().is_terminal() {
        let _ = writeln!(
            stderr,
            "nxr: --output tui requires an interactive terminal; using live output"
        );
    }
    resolved_output_mode(requested)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn non_tty_stderr_falls_back_to_live() {
        // Cursor is never a terminal.
        let mut stderr = Cursor::new(Vec::new());
        let resolved = resolve_task_output_mode(Some(TaskOutputMode::Tui), &mut stderr);
        assert_eq!(resolved, Some(TaskOutputMode::Live));
        let rendered = String::from_utf8(stderr.into_inner()).expect("utf-8");
        assert!(rendered.contains("falling back") || rendered.contains("using live"));
    }
}
