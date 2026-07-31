//! OSC 52 clipboard emission for compact failure summaries ([ADR-0173]).
//!
//! Helps operators copy failure digests from tmux/zellij panes where native
//! selection is awkward. Payloads are sanitized; never include secret values.

use std::io::{self, IsTerminal, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::diagnostics::exit;
use crate::sanitize::sanitize_terminal_text;

/// Kill-switch for failure clipboard (`off` / `0` / `false` / `no`).
pub const OSC52_ENV: &str = "NXR_OSC52";

/// Upper bound on clipboard text before OSC 52 encoding.
const MAX_CLIPBOARD_BYTES: usize = 4096;

/// One failed node or app line in a compact summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureLine {
    /// Task node or app name (untrusted; sanitized before emission).
    pub name: String,
    /// Compact status such as `exit 1`, `cancelled`, or `timed_out`.
    pub detail: String,
}

impl FailureLine {
    /// Build a line for a nonzero process exit.
    #[must_use]
    pub fn exit(name: impl Into<String>, code: i32) -> Self {
        Self {
            name: name.into(),
            detail: format!("exit {code}"),
        }
    }

    /// Build a line with an explicit status label (no exit code).
    #[must_use]
    pub fn status(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
        }
    }
}

/// Whether OSC 52 failure clipboard is enabled.
///
/// Default: enabled. Disabled when [`OSC52_ENV`] is `off` / `0` / `false` / `no`.
#[must_use]
pub fn osc52_enabled() -> bool {
    osc52_enabled_for(std::env::var(OSC52_ENV).ok().as_deref())
}

fn osc52_enabled_for(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Format a sanitized, compact multiline failure summary for the clipboard.
#[must_use]
pub fn format_failure_summary(lines: &[FailureLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut out = String::from("nxr failed\n");
    for line in lines {
        let name = sanitize_terminal_text(&line.name);
        let detail = sanitize_terminal_text(&line.detail);
        if name.is_empty() {
            continue;
        }
        out.push_str(&name);
        if !detail.is_empty() {
            out.push(' ');
            out.push_str(&detail);
        }
        out.push('\n');
    }
    if out.len() > MAX_CLIPBOARD_BYTES {
        out.truncate(MAX_CLIPBOARD_BYTES);
        while !out.is_char_boundary(MAX_CLIPBOARD_BYTES) {
            out.pop();
        }
    }
    out
}

/// Build the OSC 52 escape sequence that copies `text` to the system clipboard.
#[must_use]
pub fn osc52_clipboard_sequence(text: &str) -> String {
    let encoded = BASE64.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// Emit OSC 52 to copy `summary` when enabled and stderr is a TTY.
///
/// `summary` is sanitized before encoding (defense in depth).
///
/// # Errors
///
/// Returns an I/O error when writing to stderr fails.
pub fn emit_failure_clipboard(summary: &str) -> io::Result<()> {
    if summary.is_empty() || !osc52_enabled() || !io::stderr().is_terminal() {
        return Ok(());
    }
    let sanitized = sanitize_terminal_text(summary);
    let sequence = osc52_clipboard_sequence(&sanitized);
    let mut stderr = io::stderr().lock();
    stderr.write_all(sequence.as_bytes())?;
    stderr.flush()
}

/// Format and optionally emit OSC 52 for `lines` when `exit_code` indicates failure.
///
/// Skips emission for success (`0`) and cooperative interrupt (`10`).
///
/// # Errors
///
/// Returns an I/O error when writing to stderr fails.
pub fn maybe_emit_failure_clipboard(lines: &[FailureLine], exit_code: i32) -> io::Result<()> {
    if exit_code == exit::SUCCESS || exit_code == exit::INTERRUPTED || lines.is_empty() {
        return Ok(());
    }
    let summary = format_failure_summary(lines);
    emit_failure_clipboard(&summary)
}

#[cfg(test)]
mod tests {
    use super::{FailureLine, format_failure_summary, osc52_clipboard_sequence, osc52_enabled_for};

    #[test]
    fn kill_switch_disables_osc52() {
        assert!(!osc52_enabled_for(Some("off")));
        assert!(!osc52_enabled_for(Some("0")));
        assert!(!osc52_enabled_for(Some("false")));
        assert!(!osc52_enabled_for(Some("no")));
        assert!(!osc52_enabled_for(Some(" OFF ")));
    }

    #[test]
    fn kill_switch_allows_default_and_explicit_on() {
        assert!(osc52_enabled_for(None));
        assert!(osc52_enabled_for(Some("on")));
        assert!(osc52_enabled_for(Some("1")));
    }

    #[test]
    fn format_failure_summary_lists_nodes_and_exits() {
        let summary = format_failure_summary(&[
            FailureLine::exit("lint", 1),
            FailureLine::status("gate", "cancelled"),
        ]);
        assert_eq!(summary, "nxr failed\nlint exit 1\ngate cancelled\n");
    }

    #[test]
    fn format_failure_summary_strips_control_sequences() {
        let summary = format_failure_summary(&[FailureLine::exit("\u{1b}[31mhack\u{1b}[0m", 1)]);
        assert_eq!(summary, "nxr failed\nhack exit 1\n");
    }

    #[test]
    fn osc52_sequence_base64_encodes_payload() {
        let seq = osc52_clipboard_sequence("nxr failed\nlint exit 1\n");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
        assert!(seq.contains("bnhyIGZhaWxlZA")); // "nxr failed" prefix
    }
}
