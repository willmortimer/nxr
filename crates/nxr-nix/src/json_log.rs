//! Nom-style human formatting of Nix `--log-format internal-json` streams.
//!
//! This is the CLI progress path (not a TUI). Lines look like:
//! `@nix {"action":"start","id":1,"text":"building …","type":0}`.

use std::collections::BTreeMap;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

/// Environment variable selecting Nix progress rendering for interactive ops.
pub const NIX_PROGRESS_ENV: &str = "NXR_NIX_PROGRESS";

/// How [`crate`] formats Nix stderr for `build` / `check` / `shell`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NixProgressMode {
    /// Inherit raw Nix stderr (or non-TTY tee capture).
    Off,
    /// Parse `internal-json` and print a compact activity line.
    Builtin,
    /// Prefer `nom` on `PATH` when present; otherwise [`Self::Builtin`].
    Nom,
}

impl NixProgressMode {
    /// Resolve from `NXR_NIX_PROGRESS` (`auto` / unset → builtin on TTY).
    #[must_use]
    pub fn from_env() -> Self {
        match env::var(NIX_PROGRESS_ENV) {
            Ok(raw) => {
                let lower = raw.trim().to_ascii_lowercase();
                match lower.as_str() {
                    "" | "auto" => Self::default_for_tty(),
                    "0" | "false" | "no" | "off" => Self::Off,
                    "builtin" | "on" | "1" | "true" | "yes" => Self::Builtin,
                    "nom" => Self::Nom,
                    _ => Self::default_for_tty(),
                }
            }
            Err(_) => Self::default_for_tty(),
        }
    }

    fn default_for_tty() -> Self {
        if io::stderr().is_terminal() {
            Self::Builtin
        } else {
            Self::Off
        }
    }
}

/// Ensure argv requests Nix internal JSON logs (idempotent).
pub fn ensure_internal_json_log_format(args: &mut Vec<String>) {
    let has = args.windows(2).any(|w| w[0] == "--log-format");
    if has {
        return;
    }
    // Insert after the subcommand (`build`, `develop`, `flake`, …).
    let insert_at = if args.first().is_some_and(|a| !a.starts_with('-')) {
        1
    } else {
        0
    };
    args.insert(insert_at, "--log-format".to_owned());
    args.insert(insert_at + 1, "internal-json".to_owned());
}

/// Locate `nom` (`nix-output-monitor`) when callers request [`NixProgressMode::Nom`].
#[must_use]
pub fn locate_nom() -> Option<std::path::PathBuf> {
    static NOM: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    NOM.get_or_init(|| which::which("nom").ok()).clone()
}

#[derive(Debug, Deserialize)]
struct NixLogEvent {
    action: String,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    level: Option<i32>,
    #[serde(flatten)]
    _rest: BTreeMap<String, Value>,
}

/// Stateful formatter: tracks in-flight activities and emits status / message lines.
#[derive(Debug, Default)]
pub struct NixProgressFormatter {
    active: BTreeMap<u64, String>,
    last_status: String,
}

impl NixProgressFormatter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one stderr line. Returns human text to print (without trailing newline).
    pub fn feed_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return None;
        }

        let Some(json) = trimmed.strip_prefix("@nix ") else {
            // Non-JSON noise (rare): pass through.
            return Some(trimmed.to_owned());
        };

        let event: NixLogEvent = serde_json::from_str(json).ok()?;
        match event.action.as_str() {
            "start" => {
                let id = event.id?;
                let text = sanitize_activity(event.text.as_deref().unwrap_or("working"));
                self.active.insert(id, text);
                self.status_line()
            }
            "stop" => {
                if let Some(id) = event.id {
                    self.active.remove(&id);
                }
                self.status_line()
            }
            "msg" => {
                let msg = event.msg.as_deref()?.trim();
                if msg.is_empty() {
                    return None;
                }
                // Drop low-noise debug; keep warnings/errors and normal info.
                if event.level.is_some_and(|level| level > 1) {
                    return None;
                }
                Some(strip_ansi_basic(msg))
            }
            "result" => None,
            _ => None,
        }
    }

    fn status_line(&mut self) -> Option<String> {
        if self.active.is_empty() {
            self.last_status.clear();
            return None;
        }
        let n = self.active.len();
        let newest = self
            .active
            .values()
            .next_back()
            .cloned()
            .unwrap_or_default();
        let line = if n == 1 {
            format!("… {newest}")
        } else {
            format!("… [{n}] {newest}")
        };
        if line == self.last_status {
            return None;
        }
        self.last_status = line.clone();
        Some(line)
    }
}

/// Write a progress line to stderr, using a carriage-return update when a TTY.
///
/// Status lines (starting with `…`) stay on one row; other lines clear that row
/// and print normally.
pub fn write_progress_line(out: &mut impl Write, line: &str, is_tty: bool) -> io::Result<()> {
    let is_status = line.starts_with('…');
    if is_tty {
        write!(out, "\r\x1b[2K")?;
        if is_status {
            write!(out, "{line}")?;
        } else {
            writeln!(out, "{line}")?;
        }
    } else {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

fn sanitize_activity(text: &str) -> String {
    let flat = strip_ansi_basic(text);
    const MAX: usize = 96;
    if flat.chars().count() <= MAX {
        return flat;
    }
    let truncated: String = flat.chars().take(MAX.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn strip_ansi_basic(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\r' {
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{NixProgressFormatter, ensure_internal_json_log_format};

    #[test]
    fn injects_log_format_after_subcommand() {
        let mut args = vec!["build".to_owned(), ".#nxr".to_owned()];
        ensure_internal_json_log_format(&mut args);
        assert_eq!(
            args,
            vec![
                "build".to_owned(),
                "--log-format".to_owned(),
                "internal-json".to_owned(),
                ".#nxr".to_owned()
            ]
        );
        ensure_internal_json_log_format(&mut args);
        assert_eq!(args.iter().filter(|a| *a == "--log-format").count(), 1);
    }

    #[test]
    fn formats_start_stop_and_msg() {
        let mut fmt = NixProgressFormatter::new();
        let start = fmt.feed_line(
            r#"@nix {"action":"start","id":1,"level":0,"text":"building 'foo'","type":0}"#,
        );
        assert_eq!(start.as_deref(), Some("… building 'foo'"));
        let msg =
            fmt.feed_line(r#"@nix {"action":"msg","level":1,"msg":"warning: something happened"}"#);
        assert_eq!(msg.as_deref(), Some("warning: something happened"));
        let stop = fmt.feed_line(r#"@nix {"action":"stop","id":1}"#);
        assert!(stop.is_none());
    }
}
