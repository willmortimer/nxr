//! `nxr migrate` — suggest nxr Nix from Justfile or mise.toml (never executes recipes).

mod emit;
mod justfile;
mod mise;

pub use emit::{MigrateEmitOptions, MigrateEmitStyle};

use std::fs;
use std::io::{self, Write};

use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::diagnostics::exit;

use crate::commands::common::current_invocation_directory;
use crate::runner_output::RunnerOutput;

/// `nxr migrate` subcommands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrateSource {
    Justfile,
    Mise,
}

/// Errors while migrating external task definitions.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error("source file not found: {path}")]
    SourceMissing { path: String },
    #[error("failed to parse {path}: {message}")]
    Parse { path: String, message: String },
    #[error("no migratable entries found in {path}")]
    NoEntries { path: String },
    #[error("output path already exists: {path} (remove it or choose another path)")]
    OutputExists { path: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl MigrateError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::SourceMissing { .. } | Self::NoEntries { .. } => exit::NOT_FOUND,
            Self::Parse { .. } | Self::OutputExists { .. } => exit::USAGE,
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Inputs for `nxr migrate`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrateRequest<'a> {
    pub source: MigrateSource,
    pub input: Option<&'a Utf8Path>,
    pub write: Option<&'a Utf8Path>,
    pub emit_options: emit::MigrateEmitOptions,
}

/// Read the source file, render Nix, and print or write it.
///
/// # Errors
///
/// Returns [`MigrateError`] when the source is missing or rendering fails.
pub fn run(request: MigrateRequest<'_>, runner: RunnerOutput) -> Result<i32, MigrateError> {
    let invocation_cwd = current_invocation_directory()?;
    let input = resolve_input_path(request.source, request.input, &invocation_cwd)?;
    let contents = fs::read_to_string(&input).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            MigrateError::SourceMissing {
                path: input.to_string(),
            }
        } else {
            MigrateError::Io(error)
        }
    })?;

    let entries = match request.source {
        MigrateSource::Justfile => {
            let entries = justfile::parse_justfile_entries(&contents);
            if entries.is_empty() {
                return Err(MigrateError::NoEntries {
                    path: input.to_string(),
                });
            }
            entries
        }
        MigrateSource::Mise => {
            let entries = mise::parse_mise_entries(&contents)?;
            if entries.is_empty() {
                return Err(MigrateError::NoEntries {
                    path: input.to_string(),
                });
            }
            entries
        }
    };

    let source_label = match request.source {
        MigrateSource::Justfile => format!("justfile ({input})"),
        MigrateSource::Mise => format!("mise.toml ({input})"),
    };
    let limitations = match request.source {
        MigrateSource::Justfile => justfile::LIMITATIONS,
        MigrateSource::Mise => mise::LIMITATIONS,
    };
    let rendered = emit::render_per_system_fragment(
        &source_label,
        limitations,
        &entries,
        &request.emit_options,
    );

    if request.emit_options.emit_scripts {
        let invocation_cwd = current_invocation_directory()?;
        let scripts_root = request
            .write
            .and_then(|path| path.parent())
            .unwrap_or(invocation_cwd.as_path());
        let written = emit::write_workspace_scripts(scripts_root, &entries)?;
        for path in written {
            runner
                .info(format!("wrote {path}"))
                .map_err(MigrateError::Io)?;
        }
    }

    if let Some(path) = request.write {
        if path.exists() {
            return Err(MigrateError::OutputExists {
                path: path.to_string(),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent.as_std_path())?;
        }
        fs::write(path.as_std_path(), format!("{rendered}\n"))?;
        runner
            .info(format!("wrote {path}"))
            .map_err(MigrateError::Io)?;
        return Ok(exit::SUCCESS);
    }

    let mut stdout = io::stdout().lock();
    write!(stdout, "{rendered}")?;
    if !rendered.ends_with('\n') {
        writeln!(stdout)?;
    }
    Ok(exit::SUCCESS)
}

fn resolve_input_path(
    source: MigrateSource,
    input: Option<&Utf8Path>,
    cwd: &Utf8Path,
) -> Result<Utf8PathBuf, MigrateError> {
    if let Some(path) = input {
        return Ok(path.to_path_buf());
    }

    let candidates: &[&str] = match source {
        MigrateSource::Justfile => &["Justfile", "justfile"],
        MigrateSource::Mise => &["mise.toml"],
    };

    for candidate in candidates {
        let path = cwd.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }

    Err(MigrateError::SourceMissing {
        path: candidates[0].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::{MigrateRequest, MigrateSource, run};
    use crate::output_options::{ColorWhen, LogFormat, OutputOptions};
    use crate::runner_output::RunnerOutput;

    #[test]
    fn migrate_justfile_to_stdout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let justfile = Utf8PathBuf::from_path_buf(temp.path().join("Justfile")).expect("utf-8");
        std::fs::write(justfile.as_std_path(), "hello:\n    echo hello\n").expect("write justfile");

        let runner = RunnerOutput::new(OutputOptions::new(
            0,
            0,
            true,
            false,
            ColorWhen::Never,
            LogFormat::Plain,
        ));
        let code = run(
            MigrateRequest {
                source: MigrateSource::Justfile,
                input: Some(justfile.as_path()),
                write: None,
                emit_options: Default::default(),
            },
            runner,
        )
        .expect("migrate");
        assert_eq!(code, 0);
    }
}
