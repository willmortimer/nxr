//! Global CLI output and color options.

use std::io::IsTerminal;

use clap::ValueEnum;

use crate::output_task::{EventsFormat, TaskOutputMode};

/// Human presentation and machine event transport are independent of verbosity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationMode {
    Raw,
    Compact,
    Live,
    Grouped,
    Failures,
    Summary,
    Tui,
    JsonLines,
}

/// Increasing diagnostic detail without changing the selected presentation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
    Logs,
    Trace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturePolicy {
    Disabled,
    Ephemeral,
    Durable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamDisposition {
    InheritBytes,
    CaptureBytes,
    DataOutput,
    DiagnosticOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    None,
    UntilExit,
    Durable,
}

/// Caller-observed capabilities; the resolver itself does not inspect the OS.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
    pub color: bool,
    pub unicode: bool,
    pub fullscreen: bool,
}

/// All execution facts needed to resolve output before spawning a child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputPolicyContext {
    pub requested: Option<TaskOutputMode>,
    pub events: Option<EventsFormat>,
    pub jobs: usize,
    pub interactive: bool,
    /// The child owns stdout as data (for example JSON or binary export).
    pub stdout_data: bool,
    pub durable: bool,
    pub background: bool,
    pub ci: bool,
    pub terminal: TerminalCapabilities,
}

impl Default for OutputPolicyContext {
    fn default() -> Self {
        Self {
            requested: None,
            events: None,
            jobs: 1,
            interactive: false,
            stdout_data: false,
            durable: false,
            background: false,
            ci: false,
            terminal: TerminalCapabilities::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OutputPolicyError {
    #[error("--output raw cannot be combined with parallel jobs or --events")]
    RawMultiplexed,
    #[error(
        "interactive tasks require inherited terminal streams and cannot use multiplexed output or --events"
    )]
    InteractiveMultiplexed,
    #[error("interactive tasks cannot run in the background")]
    InteractiveBackground,
    #[error("data stdout requires serial raw output without --events")]
    DataMultiplexed,
    #[error("--output tui cannot share the terminal with --events")]
    TuiEvents,
}

/// One resolved policy shared by task, runner and frontend adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedOutputPolicy {
    pub presentation: PresentationMode,
    pub verbosity: Verbosity,
    pub capture: CapturePolicy,
    pub stdout: StreamDisposition,
    pub stderr: StreamDisposition,
    pub terminal: TerminalCapabilities,
    pub retention: RetentionPolicy,
}

impl ResolvedOutputPolicy {
    /// Preserve legacy serial/parallel defaults; compact remains opt-in.
    pub fn resolve(
        options: OutputOptions,
        context: OutputPolicyContext,
    ) -> Result<Self, OutputPolicyError> {
        if context.interactive && context.background {
            return Err(OutputPolicyError::InteractiveBackground);
        }
        if context.requested == Some(TaskOutputMode::Raw)
            && (context.jobs > 1 || context.events.is_some())
        {
            return Err(OutputPolicyError::RawMultiplexed);
        }
        if context.interactive
            && (context
                .requested
                .is_some_and(TaskOutputMode::uses_piped_stdio)
                || context.events.is_some())
        {
            return Err(OutputPolicyError::InteractiveMultiplexed);
        }
        if context.stdout_data
            && (context.jobs > 1
                || context.events.is_some()
                || context
                    .requested
                    .is_some_and(|mode| mode != TaskOutputMode::Raw))
        {
            return Err(OutputPolicyError::DataMultiplexed);
        }
        if context.requested == Some(TaskOutputMode::Tui) && context.events.is_some() {
            return Err(OutputPolicyError::TuiEvents);
        }

        let verbosity = options.verbosity();
        let requested = context.requested.unwrap_or(if context.jobs > 1 {
            TaskOutputMode::Live
        } else {
            TaskOutputMode::Raw
        });
        let presentation = match requested {
            TaskOutputMode::Raw if context.events.is_some() => PresentationMode::JsonLines,
            TaskOutputMode::Raw => PresentationMode::Raw,
            TaskOutputMode::Compact => PresentationMode::Compact,
            TaskOutputMode::Live => PresentationMode::Live,
            TaskOutputMode::Grouped => PresentationMode::Grouped,
            TaskOutputMode::Failures => PresentationMode::Failures,
            TaskOutputMode::Summary => PresentationMode::Summary,
            TaskOutputMode::Tui
                if context.ci
                    || context.background
                    || !context.terminal.stderr_tty
                    || !context.terminal.fullscreen =>
            {
                PresentationMode::Live
            }
            TaskOutputMode::Tui => PresentationMode::Tui,
        };
        let raw = presentation == PresentationMode::Raw;
        let capture = if context.durable || context.background {
            CapturePolicy::Durable
        } else if raw {
            CapturePolicy::Disabled
        } else {
            CapturePolicy::Ephemeral
        };
        let mut terminal = context.terminal;
        terminal.color &= !options.plain
            && !options.no_color
            && options.color != ColorWhen::Never
            && options.effective_log_format() == LogFormat::Human;
        terminal.unicode &= !options.plain;
        Ok(Self {
            presentation,
            verbosity,
            capture,
            stdout: if context.stdout_data {
                StreamDisposition::DataOutput
            } else if raw {
                StreamDisposition::InheritBytes
            } else {
                StreamDisposition::CaptureBytes
            },
            stderr: if raw {
                StreamDisposition::InheritBytes
            } else {
                StreamDisposition::DiagnosticOutput
            },
            terminal,
            retention: match capture {
                CapturePolicy::Disabled => RetentionPolicy::None,
                CapturePolicy::Ephemeral => RetentionPolicy::UntilExit,
                CapturePolicy::Durable => RetentionPolicy::Durable,
            },
        })
    }

    /// Adapter for the existing task sink; event-only transport has no renderer.
    #[must_use]
    pub const fn task_output_mode(self) -> Option<TaskOutputMode> {
        match self.presentation {
            PresentationMode::Raw => Some(TaskOutputMode::Raw),
            PresentationMode::Compact => Some(TaskOutputMode::Compact),
            PresentationMode::Live => Some(TaskOutputMode::Live),
            PresentationMode::Grouped => Some(TaskOutputMode::Grouped),
            PresentationMode::Failures => Some(TaskOutputMode::Failures),
            PresentationMode::Summary => Some(TaskOutputMode::Summary),
            PresentationMode::Tui => Some(TaskOutputMode::Tui),
            PresentationMode::JsonLines => None,
        }
    }
}

/// When the runner may emit ANSI color on diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

/// Format for runner diagnostics written to stderr.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum LogFormat {
    #[default]
    Human,
    Plain,
    Json,
}

/// Parsed global output flags from the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputOptions {
    pub quiet: u8,
    pub verbose: u8,
    pub plain: bool,
    pub no_color: bool,
    pub color: ColorWhen,
    pub log_format: LogFormat,
}

impl OutputOptions {
    #[must_use]
    pub const fn new(
        quiet: u8,
        verbose: u8,
        plain: bool,
        no_color: bool,
        color: ColorWhen,
        log_format: LogFormat,
    ) -> Self {
        Self {
            quiet,
            verbose,
            plain,
            no_color,
            color,
            log_format,
        }
    }

    #[must_use]
    pub const fn is_quiet(self) -> bool {
        self.quiet > 0
    }

    #[must_use]
    pub const fn is_verbose(self) -> bool {
        self.verbose > 0
    }

    /// Quiet takes precedence while presentation is resolved independently.
    #[must_use]
    pub const fn verbosity(self) -> Verbosity {
        if !self.show_runner_info() {
            Verbosity::Quiet
        } else if !self.is_verbose() {
            Verbosity::Normal
        } else {
            match self.verbose {
                1 => Verbosity::Verbose,
                2 => Verbosity::Logs,
                _ => Verbosity::Trace,
            }
        }
    }

    #[must_use]
    pub fn color_enabled(self) -> bool {
        if self.plain
            || self.no_color
            || matches!(self.log_format, LogFormat::Json | LogFormat::Plain)
        {
            return false;
        }

        match self.color {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => std::io::stderr().is_terminal(),
        }
    }

    #[must_use]
    pub const fn show_runner_info(self) -> bool {
        !self.is_quiet()
    }

    #[must_use]
    pub const fn effective_log_format(self) -> LogFormat {
        if self.plain && matches!(self.log_format, LogFormat::Human) {
            LogFormat::Plain
        } else {
            self.log_format
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> OutputOptions {
        OutputOptions::new(0, 0, false, false, ColorWhen::Auto, LogFormat::Human)
    }

    #[test]
    fn policy_preserves_serial_and_parallel_defaults() {
        let serial = ResolvedOutputPolicy::resolve(options(), OutputPolicyContext::default())
            .expect("serial policy");
        assert_eq!(serial.presentation, PresentationMode::Raw);
        assert_eq!(serial.capture, CapturePolicy::Disabled);
        assert_eq!(serial.stdout, StreamDisposition::InheritBytes);
        let parallel = ResolvedOutputPolicy::resolve(
            options(),
            OutputPolicyContext {
                jobs: 4,
                ..OutputPolicyContext::default()
            },
        )
        .expect("parallel policy");
        assert_eq!(parallel.presentation, PresentationMode::Live);
    }

    #[test]
    fn verbosity_never_changes_explicit_presentation() {
        for verbose in 0..=3 {
            let mut flags = options();
            flags.verbose = verbose;
            let policy = ResolvedOutputPolicy::resolve(
                flags,
                OutputPolicyContext {
                    requested: Some(TaskOutputMode::Compact),
                    ..OutputPolicyContext::default()
                },
            )
            .expect("compact policy");
            assert_eq!(policy.presentation, PresentationMode::Compact);
            assert_eq!(policy.stdout, StreamDisposition::CaptureBytes);
        }
    }

    #[test]
    fn quiet_wins_over_diagnostic_verbosity() {
        let mut flags = options();
        flags.quiet = 1;
        flags.verbose = 3;
        let policy = ResolvedOutputPolicy::resolve(flags, OutputPolicyContext::default())
            .expect("quiet policy");
        assert_eq!(policy.verbosity, Verbosity::Quiet);
        assert_eq!(policy.presentation, PresentationMode::Raw);
    }

    #[test]
    fn raw_refuses_parallel_and_events() {
        for context in [
            OutputPolicyContext {
                requested: Some(TaskOutputMode::Raw),
                jobs: 2,
                ..OutputPolicyContext::default()
            },
            OutputPolicyContext {
                requested: Some(TaskOutputMode::Raw),
                events: Some(EventsFormat::Jsonl),
                ..OutputPolicyContext::default()
            },
        ] {
            assert_eq!(
                ResolvedOutputPolicy::resolve(options(), context),
                Err(OutputPolicyError::RawMultiplexed)
            );
        }
    }

    #[test]
    fn interactive_refuses_capture_and_background() {
        let context = OutputPolicyContext {
            interactive: true,
            requested: Some(TaskOutputMode::Compact),
            ..OutputPolicyContext::default()
        };
        assert_eq!(
            ResolvedOutputPolicy::resolve(options(), context),
            Err(OutputPolicyError::InteractiveMultiplexed)
        );
        let context = OutputPolicyContext {
            interactive: true,
            background: true,
            ..OutputPolicyContext::default()
        };
        assert_eq!(
            ResolvedOutputPolicy::resolve(options(), context),
            Err(OutputPolicyError::InteractiveBackground)
        );
    }

    #[test]
    fn data_stdout_is_inherited_without_decoration() {
        let context = OutputPolicyContext {
            stdout_data: true,
            ..OutputPolicyContext::default()
        };
        let policy = ResolvedOutputPolicy::resolve(options(), context).expect("data policy");
        assert_eq!(policy.presentation, PresentationMode::Raw);
        assert_eq!(policy.stdout, StreamDisposition::DataOutput);
        let context = OutputPolicyContext {
            requested: Some(TaskOutputMode::Compact),
            ..context
        };
        assert_eq!(
            ResolvedOutputPolicy::resolve(options(), context),
            Err(OutputPolicyError::DataMultiplexed)
        );
    }

    #[test]
    fn event_only_transport_and_retention_are_explicit() {
        let policy = ResolvedOutputPolicy::resolve(
            options(),
            OutputPolicyContext {
                events: Some(EventsFormat::Jsonl),
                durable: true,
                ..OutputPolicyContext::default()
            },
        )
        .expect("event policy");
        assert_eq!(policy.presentation, PresentationMode::JsonLines);
        assert_eq!(policy.task_output_mode(), None);
        assert_eq!(policy.capture, CapturePolicy::Durable);
        assert_eq!(policy.retention, RetentionPolicy::Durable);
    }

    #[test]
    fn tui_keeps_legacy_live_fallback_without_terminal_ownership() {
        let context = OutputPolicyContext {
            requested: Some(TaskOutputMode::Tui),
            ..OutputPolicyContext::default()
        };
        let policy = ResolvedOutputPolicy::resolve(options(), context).expect("fallback");
        assert_eq!(policy.presentation, PresentationMode::Live);
        let context = OutputPolicyContext {
            terminal: TerminalCapabilities {
                stderr_tty: true,
                fullscreen: true,
                ..TerminalCapabilities::default()
            },
            ..context
        };
        assert_eq!(
            ResolvedOutputPolicy::resolve(options(), context)
                .expect("tui")
                .presentation,
            PresentationMode::Tui
        );
        assert_eq!(
            ResolvedOutputPolicy::resolve(
                options(),
                OutputPolicyContext {
                    ci: true,
                    ..context
                }
            )
            .expect("CI fallback")
            .presentation,
            PresentationMode::Live
        );
    }

    #[test]
    fn plain_suppresses_terminal_decoration_without_changing_mode() {
        let mut flags = options();
        flags.plain = true;
        let policy = ResolvedOutputPolicy::resolve(
            flags,
            OutputPolicyContext {
                requested: Some(TaskOutputMode::Compact),
                terminal: TerminalCapabilities {
                    color: true,
                    unicode: true,
                    ..TerminalCapabilities::default()
                },
                ..OutputPolicyContext::default()
            },
        )
        .expect("plain compact");
        assert!(!policy.terminal.color);
        assert!(!policy.terminal.unicode);
        assert_eq!(policy.presentation, PresentationMode::Compact);
    }

    #[test]
    fn quiet_hides_runner_info() {
        let options = OutputOptions::new(1, 0, false, true, ColorWhen::Never, LogFormat::Human);
        assert!(!options.show_runner_info());
    }

    #[test]
    fn plain_and_no_color_disable_color() {
        let plain = OutputOptions::new(0, 0, true, false, ColorWhen::Always, LogFormat::Human);
        assert!(!plain.color_enabled());

        let no_color = OutputOptions::new(0, 0, false, true, ColorWhen::Always, LogFormat::Human);
        assert!(!no_color.color_enabled());
    }

    #[test]
    fn json_log_format_disables_color() {
        let options = OutputOptions::new(0, 0, false, false, ColorWhen::Always, LogFormat::Json);
        assert!(!options.color_enabled());
    }
}
