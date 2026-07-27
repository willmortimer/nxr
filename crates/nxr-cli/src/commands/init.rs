//! `nxr init` — scaffold flake templates for new consumers.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal};

use camino::{Utf8Path, Utf8PathBuf};
use dialoguer::Confirm;
use nxr_core::diagnostics::exit;

use crate::commands::common::current_invocation_directory;
use crate::runner_output::RunnerOutput;

/// Supported `nxr init` templates (ADR-0148).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitTemplate {
    Rust,
    Node,
    Mixed,
    Monorepo,
}

impl InitTemplate {
    /// All templates in stable display order.
    pub const ALL: [Self; 4] = [Self::Rust, Self::Node, Self::Mixed, Self::Monorepo];

    /// Parse a template name from CLI input.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "rust" => Some(Self::Rust),
            "node" => Some(Self::Node),
            "mixed" => Some(Self::Mixed),
            "monorepo" => Some(Self::Monorepo),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Mixed => "mixed",
            Self::Monorepo => "monorepo",
        }
    }

    #[must_use]
    pub fn files(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Rust => &[(
                "flake.nix",
                include_str!("../../../../templates/rust/flake.nix"),
            )],
            Self::Node => &[(
                "flake.nix",
                include_str!("../../../../templates/node/flake.nix"),
            )],
            Self::Mixed => &[(
                "flake.nix",
                include_str!("../../../../templates/mixed/flake.nix"),
            )],
            Self::Monorepo => &[
                (
                    "flake.nix",
                    include_str!("../../../../templates/monorepo/flake.nix"),
                ),
                (
                    "nxr.projects.json",
                    include_str!("../../../../templates/monorepo/nxr.projects.json"),
                ),
            ],
        }
    }
}

/// Errors while scaffolding a template.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error(transparent)]
    Prepare(#[from] crate::commands::common::PrepareError),
    #[error("unknown template `{name}` (choose: {choices})")]
    UnknownTemplate { name: String, choices: String },
    #[error("template name is required (choose: {choices})")]
    MissingTemplate { choices: String },
    #[error("target path already exists: {path}")]
    TargetExists { path: String },
    #[error("interactive confirmation requires a terminal (stdin and stderr must be TTYs)")]
    NoTty,
    #[error("scaffold cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("interactive prompt failed: {0}")]
    Prompt(#[from] dialoguer::Error),
}

impl InitError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::UnknownTemplate { .. } | Self::MissingTemplate { .. } => exit::USAGE,
            Self::TargetExists { .. } | Self::NoTty | Self::Cancelled | Self::Prompt(_) => {
                exit::USAGE
            }
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Inputs for `nxr init`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitRequest<'a> {
    pub template: Option<&'a str>,
    pub target_dir: Option<&'a Utf8Path>,
    pub yes: bool,
}

/// Write the selected template into `target_dir`.
///
/// # Errors
///
/// Returns [`InitError`] when validation, confirmation, or I/O fails.
pub fn run(request: InitRequest<'_>, runner: RunnerOutput) -> Result<i32, InitError> {
    let invocation_cwd = current_invocation_directory()?;
    let target_dir = request
        .target_dir
        .map(Utf8PathBuf::from)
        .unwrap_or(invocation_cwd);

    let template = match request.template {
        Some(name) => InitTemplate::parse(name).ok_or_else(|| InitError::UnknownTemplate {
            name: name.to_owned(),
            choices: template_choices(),
        })?,
        None => {
            return Err(InitError::MissingTemplate {
                choices: template_choices(),
            });
        }
    };

    let files = template.files();
    let mut planned = BTreeMap::<Utf8PathBuf, &'static str>::new();
    for (relative, content) in files {
        let path = target_dir.join(relative);
        if path.exists() {
            return Err(InitError::TargetExists {
                path: path.to_string(),
            });
        }
        planned.insert(path, *content);
    }

    if !request.yes {
        ensure_interactive_terminal()?;
        let summary = planned
            .keys()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Write the `{template}` template to {}?\n{summary}",
            target_dir.as_str(),
            template = template.as_str()
        );
        let confirmed = Confirm::new()
            .with_prompt(prompt)
            .default(true)
            .interact()
            .map_err(InitError::Prompt)?;
        if !confirmed {
            return Err(InitError::Cancelled);
        }
    }

    if let Some(parent) = target_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    for (path, content) in planned {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent.as_std_path())?;
        }
        fs::write(path.as_std_path(), format!("{content}\n"))?;
        runner
            .info(format!("wrote {path}"))
            .map_err(InitError::Io)?;
    }

    Ok(exit::SUCCESS)
}

fn template_choices() -> String {
    InitTemplate::ALL
        .iter()
        .map(|template| template.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn ensure_interactive_terminal() -> Result<(), InitError> {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(InitError::NoTty)
    }
}

#[cfg(test)]
mod tests {
    use super::{InitTemplate, template_choices};

    #[test]
    fn template_names_are_stable() {
        assert_eq!(InitTemplate::parse("rust"), Some(InitTemplate::Rust));
        assert_eq!(InitTemplate::parse("node"), Some(InitTemplate::Node));
        assert_eq!(InitTemplate::parse("mixed"), Some(InitTemplate::Mixed));
        assert_eq!(
            InitTemplate::parse("monorepo"),
            Some(InitTemplate::Monorepo)
        );
        assert!(InitTemplate::parse("default").is_none());
        assert!(template_choices().contains("rust"));
    }

    #[test]
    fn embedded_templates_include_flake_nix() {
        for template in InitTemplate::ALL {
            let files = template.files();
            assert!(
                files.iter().any(|(name, _)| *name == "flake.nix"),
                "{} must ship flake.nix",
                template.as_str()
            );
            let flake = files
                .iter()
                .find(|(name, _)| *name == "flake.nix")
                .map(|(_, content)| *content)
                .expect("flake.nix");
            assert!(flake.contains("nxr.flakeModules.default"));
        }
    }
}
