//! Supported shell targets for `nxr completion`.

use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;

/// Shell for which `nxr completion` can emit a script.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Shell {
    /// GNU Bash completion.
    Bash,
    /// Zsh completion.
    Zsh,
    /// Fish completion.
    Fish,
}

impl Shell {
    #[must_use]
    pub const fn asset_file_name(self) -> &'static str {
        match self {
            Self::Bash => "nxr.bash",
            Self::Zsh => "nxr.zsh",
            Self::Fish => "nxr.fish",
        }
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        })
    }
}

impl FromStr for Shell {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            other => Err(format!("unsupported shell: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Shell;
    use clap::ValueEnum;

    #[test]
    fn shell_value_variants_match_contract() {
        assert_eq!(
            Shell::value_variants(),
            &[Shell::Bash, Shell::Zsh, Shell::Fish]
        );
    }

    #[test]
    fn shell_display_and_parse_round_trip() {
        for shell in Shell::value_variants() {
            let rendered = shell.to_string();
            let parsed: Shell = rendered.parse().expect("parse shell");
            assert_eq!(parsed, *shell);
        }
    }
}
