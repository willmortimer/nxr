//! Runner diagnostics on stderr (never app stdout during `run`).

use std::io::{self, IsTerminal, Write};

use nxr_core::sanitize::sanitize_terminal_text;
use serde_json::json;

use crate::output_options::{LogFormat, OutputOptions, ResolvedOutputPolicy, Verbosity};

/// Emit runner-originated diagnostics to stderr according to output options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunnerOutput {
    options: OutputOptions,
    verbosity: Verbosity,
}

impl RunnerOutput {
    #[must_use]
    pub const fn new(options: OutputOptions) -> Self {
        let verbosity = options.verbosity();
        Self { options, verbosity }
    }

    /// Apply the shared verbosity and terminal decision to runner diagnostics.
    #[must_use]
    pub fn with_policy(mut self, policy: &ResolvedOutputPolicy) -> Self {
        self.verbosity = policy.verbosity;
        self.options.no_color |= !policy.terminal.color;
        self
    }

    #[must_use]
    pub const fn options(self) -> OutputOptions {
        self.options
    }

    /// Direct applications own their byte streams; only runner diagnostics use verbosity.
    pub fn direct_output_policy(self) -> ResolvedOutputPolicy {
        use crate::output_options::{OutputPolicyContext, TerminalCapabilities};
        ResolvedOutputPolicy::resolve(
            self.options,
            OutputPolicyContext {
                stdout_data: true,
                terminal: TerminalCapabilities {
                    stdin_tty: io::stdin().is_terminal(),
                    stdout_tty: io::stdout().is_terminal(),
                    stderr_tty: io::stderr().is_terminal(),
                    color: self.options.color_enabled(),
                    unicode: !self.options.plain,
                    fullscreen: false,
                },
                ..Default::default()
            },
        )
        .expect("single direct application output policy has no multiplexing conflicts")
    }

    /// Info-level runner message (suppressed in quiet mode).
    ///
    /// # Errors
    ///
    /// Returns an I/O error when writing to stderr fails.
    pub fn info(self, message: impl AsRef<str>) -> io::Result<()> {
        if self.verbosity == Verbosity::Quiet {
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
        if self.verbosity < Verbosity::Verbose {
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
        match self.options.effective_log_format() {
            LogFormat::Json => {
                let line = json!({
                    "level": level,
                    "message": sanitized,
                });
                writeln!(stderr, "{line}")
            }
            LogFormat::Plain | LogFormat::Human => {
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
    }
}

#[cfg(test)]
mod tests {
    use super::RunnerOutput;
    use crate::output_options::{ColorWhen, LogFormat, OutputOptions};

    #[test]
    fn quiet_suppresses_info_but_not_error() {
        let quiet = RunnerOutput::new(OutputOptions::new(
            1,
            0,
            false,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ));
        assert!(quiet.info("listing apps").is_ok());
        assert!(quiet.error("something failed").is_ok());
    }

    #[test]
    fn verbose_requires_flag() {
        let normal = RunnerOutput::new(OutputOptions::new(
            0,
            0,
            false,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ));
        assert!(normal.verbose("discovering apps").is_ok());

        let verbose = RunnerOutput::new(OutputOptions::new(
            0,
            1,
            false,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ));
        assert!(verbose.verbose("discovering apps").is_ok());
    }
}
