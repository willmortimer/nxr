//! `nxr envrc` — generate direnv `.envrc` content (never activates direnv).

use std::fs;
use std::io::{self, Write};

use nxr_core::diagnostics::exit;

use crate::commands::common::current_invocation_directory;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while generating or writing `.envrc` content.
#[derive(Debug, thiserror::Error)]
pub enum EnvrcError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(".envrc already exists at {path} (use --force to overwrite)")]
    EnvrcExists { path: String },
    #[error("flake root is not a local path; cannot write .envrc")]
    RemoteFlake,
}

impl EnvrcError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::EnvrcExists { .. } | Self::RemoteFlake => exit::USAGE,
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Inputs for `nxr envrc`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvrcRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub write: bool,
    pub force: bool,
}

/// Render `.envrc` lines for the selected flake (and optional shell).
#[must_use]
pub fn render_envrc_content(shell: Option<&str>) -> String {
    let mut lines = Vec::new();
    match shell {
        Some(name) => lines.push(format!("use flake .#{name}")),
        None => lines.push("use flake".to_owned()),
    }
    lines.push(String::new());
    lines.push(
        "# Optional: load nxr shell completions when direnv activates this directory.".to_owned(),
    );
    lines.push(
        "# nxrd: prefer Home Manager `services.nxrd.enable` or `nxr daemon start` — do not start the daemon from direnv (wrong lifecycle).".to_owned(),
    );
    lines.join("\n")
}

/// Print or write `.envrc` for the selected flake.
///
/// Never calls `direnv allow` and never emits secret values.
///
/// # Errors
///
/// Returns [`EnvrcError`] when flake resolution or file I/O fails.
pub fn run(request: EnvrcRequest<'_>, runner: RunnerOutput) -> Result<i32, EnvrcError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let content = render_envrc_content(request.shell);

    if request.write {
        let root = flake.local_root.as_ref().ok_or(EnvrcError::RemoteFlake)?;
        let path = root.join(".envrc");
        if path.is_file() && !request.force {
            return Err(EnvrcError::EnvrcExists {
                path: path.to_string(),
            });
        }
        fs::write(path.as_std_path(), format!("{content}\n"))?;
        runner
            .info(format!("wrote {path}"))
            .map_err(EnvrcError::Io)?;
        return Ok(exit::SUCCESS);
    }

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{content}")?;
    Ok(exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::render_envrc_content;

    #[test]
    fn render_envrc_default_and_named_shell() {
        let default = render_envrc_content(None);
        assert!(default.starts_with("use flake\n"));
        assert!(default.contains("completions"));

        let backend = render_envrc_content(Some("backend"));
        assert!(backend.starts_with("use flake .#backend\n"));
    }
}
