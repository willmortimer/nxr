//! User-facing error rendering with sanitization.

use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::NixError;

/// Format a [`NixError`] with actionable context for terminal display.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_nix_error(error: &NixError) -> String {
    sanitize_terminal_text(&error.user_message())
}

/// Sanitize any error's display text for terminal output.
#[must_use]
pub fn format_error_message(error: &dyn std::error::Error) -> String {
    sanitize_terminal_text(&error.to_string())
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use nxr_nix::capabilities::NixFailureKind;

    use super::format_nix_error;
    use nxr_nix::NixError;

    #[test]
    fn nix_command_failed_includes_command_and_sanitized_stderr() {
        let error = NixError::CommandFailed {
            nix: Utf8PathBuf::from("/bin/nix"),
            args: vec![
                "flake".to_owned(),
                "show".to_owned(),
                "--json".to_owned(),
                ".".to_owned(),
            ],
            status: Some(1),
            stderr: "\u{1b}[31munknown flake\u{1b}[0m".to_owned(),
            kind: NixFailureKind::Evaluation,
        };

        let message = format_nix_error(&error);
        assert!(message.contains("failed to evaluate flake"));
        assert!(message.contains("nix flake show --json ."));
        assert!(message.contains("exited with status 1"));
        assert!(message.contains("unknown flake"));
        assert!(!message.contains('\u{1b}'));
    }
}
