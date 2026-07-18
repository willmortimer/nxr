//! Runner diagnostics on stderr (never app stdout during `run`).

use std::io::{self, Write};

use nxr_core::sanitize::sanitize_terminal_text;

use crate::output_options::OutputOptions;

/// Emit runner-originated diagnostics to stderr according to output options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunnerOutput {
    options: OutputOptions,
}

impl RunnerOutput {
    #[must_use]
    pub const fn new(options: OutputOptions) -> Self {
        Self { options }
    }

    /// Info-level runner message (suppressed in quiet mode).
    ///
    /// # Errors
    ///
    /// Returns an I/O error when writing to stderr fails.
    pub fn info(self, message: impl AsRef<str>) -> io::Result<()> {
        if !self.options.show_runner_info() {
            return Ok(());
        }
        self.write_stderr("info", message.as_ref())
    }

    /// Verbose runner diagnostics (requires at least one `-v`).
    ///
    /// # Errors
    ///
    /// Returns an I/O error when writing to stderr fails.
    pub fn verbose(self, message: impl AsRef<str>) -> io::Result<()> {
        if !self.options.is_verbose() {
            return Ok(());
        }
        self.write_stderr("verbose", message.as_ref())
    }

    /// Error-level runner message (always shown).
    ///
    /// # Errors
    ///
    /// Returns an I/O error when writing to stderr fails.
    pub fn error(self, message: impl AsRef<str>) -> io::Result<()> {
        self.write_stderr("error", message.as_ref())
    }

    fn write_stderr(self, level: &str, message: &str) -> io::Result<()> {
        let sanitized = sanitize_terminal_text(message);
        let mut stderr = io::stderr().lock();
        if self.options.color_enabled() {
            let prefix = match level {
                "error" => "\u{1b}[31merror\u{1b}[0m",
                "verbose" => "\u{1b}[2mverbose\u{1b}[0m",
                _ => "info",
            };
            writeln!(stderr, "{prefix}: {sanitized}")
        } else {
            writeln!(stderr, "{level}: {sanitized}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunnerOutput;
    use crate::output_options::{ColorWhen, OutputOptions};

    #[test]
    fn quiet_suppresses_info_but_not_error() {
        let quiet = RunnerOutput::new(OutputOptions::new(1, 0, false, true, ColorWhen::Never));
        assert!(quiet.info("listing apps").is_ok());
        assert!(quiet.error("something failed").is_ok());
    }

    #[test]
    fn verbose_requires_flag() {
        let normal = RunnerOutput::new(OutputOptions::new(0, 0, false, true, ColorWhen::Never));
        assert!(normal.verbose("discovering apps").is_ok());

        let verbose = RunnerOutput::new(OutputOptions::new(0, 1, false, true, ColorWhen::Never));
        assert!(verbose.verbose("discovering apps").is_ok());
    }
}
