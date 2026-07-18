//! Static clap completion generation plus dynamic shell hooks.

use std::io::{self, Write};

use clap::Command;
use clap_complete::generate;
use clap_complete::shells::{Bash, Fish, Zsh};

use crate::shell::Shell;

/// Emit a full completion script for `shell` to `writer`.
///
/// The script contains clap-generated static completions followed by the
/// dynamic app-completion hooks from `shell/`.
///
/// # Errors
///
/// Returns an I/O error when writing fails.
pub fn generate_script(shell: Shell, cmd: &mut Command, writer: &mut dyn Write) -> io::Result<()> {
    match shell {
        Shell::Bash => {
            generate(Bash, cmd, "nxr", writer);
            writeln!(writer)?;
            writer.write_all(dynamic_snippet(shell).as_bytes())?;
        }
        Shell::Zsh => {
            generate(Zsh, cmd, "nxr", writer);
            writeln!(writer)?;
            writer.write_all(dynamic_snippet(shell).as_bytes())?;
        }
        Shell::Fish => {
            generate(Fish, cmd, "nxr", writer);
            writeln!(writer)?;
            writer.write_all(dynamic_snippet(shell).as_bytes())?;
        }
    }

    Ok(())
}

fn dynamic_snippet(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => include_str!("../../../shell/nxr.bash"),
        Shell::Zsh => include_str!("../../../shell/nxr.zsh"),
        Shell::Fish => include_str!("../../../shell/nxr.fish"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::{CommandFactory, Parser, ValueEnum};

    use super::generate_script;
    use crate::shell::Shell;

    #[derive(Parser)]
    #[command(name = "nxr")]
    struct SampleCli {
        #[arg(long)]
        json: bool,
    }

    #[test]
    fn generate_script_smoke_contains_command_name() {
        let mut cmd = SampleCli::command();
        for shell in Shell::value_variants() {
            let mut buffer = Cursor::new(Vec::new());
            generate_script(*shell, &mut cmd, &mut buffer).expect("generate");
            let script = String::from_utf8(buffer.into_inner()).expect("utf-8 script");
            assert!(script.contains("nxr"), "{shell} script should mention nxr");
        }
    }
}
