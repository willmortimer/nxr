//! Global CLI output and color options.

use std::io::IsTerminal;

use clap::ValueEnum;

/// When the runner may emit ANSI color on diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

/// Parsed global output flags from the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputOptions {
    pub quiet: u8,
    pub verbose: u8,
    pub plain: bool,
    pub no_color: bool,
    pub color: ColorWhen,
}

impl OutputOptions {
    #[must_use]
    pub const fn new(
        quiet: u8,
        verbose: u8,
        plain: bool,
        no_color: bool,
        color: ColorWhen,
    ) -> Self {
        Self {
            quiet,
            verbose,
            plain,
            no_color,
            color,
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

    #[must_use]
    pub fn color_enabled(self) -> bool {
        if self.plain || self.no_color {
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
}

#[cfg(test)]
mod tests {
    use super::{ColorWhen, OutputOptions};

    #[test]
    fn quiet_hides_runner_info() {
        let options = OutputOptions::new(1, 0, false, true, ColorWhen::Never);
        assert!(!options.show_runner_info());
    }

    #[test]
    fn plain_and_no_color_disable_color() {
        let plain = OutputOptions::new(0, 0, true, false, ColorWhen::Always);
        assert!(!plain.color_enabled());

        let no_color = OutputOptions::new(0, 0, false, true, ColorWhen::Always);
        assert!(!no_color.color_enabled());
    }
}
