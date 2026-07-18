//! Interactive fuzzy app selector.

use std::io::{self, IsTerminal};

use dialoguer::FuzzySelect;
use nxr_core::App;
use nxr_core::diagnostics::exit;

use crate::commands::common::{DiscoverRequest, PrepareError, discover_apps};

/// Errors while running the interactive selector.
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    #[error("interactive selection requires a terminal (stdin and stderr must be TTYs)")]
    NoTty,
    #[error("no apps discovered for the selected flake")]
    NoApps,
    #[error("selection cancelled")]
    Cancelled,
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error("interactive selector failed: {0}")]
    Prompt(#[from] dialoguer::Error),
}

impl SelectError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::NoTty | Self::NoApps | Self::Cancelled | Self::Prompt(_) => exit::USAGE,
        }
    }
}

/// Discover apps and prompt the user to pick one by name.
///
/// # Errors
///
/// Returns [`SelectError`] when the environment is non-interactive, discovery
/// fails, no apps exist, or the user cancels the prompt.
pub fn pick_app_name(request: DiscoverRequest<'_>) -> Result<String, SelectError> {
    ensure_interactive_terminal()?;

    let discovered = discover_apps(request)?;
    if discovered.apps.is_empty() {
        return Err(SelectError::NoApps);
    }

    let labels = discovered
        .apps
        .iter()
        .map(format_app_label)
        .collect::<Vec<_>>();
    let selection = FuzzySelect::new()
        .with_prompt("Select app")
        .items(&labels)
        .default(0)
        .interact_opt()
        .map_err(SelectError::Prompt)?;

    let Some(index) = selection else {
        return Err(SelectError::Cancelled);
    };

    Ok(discovered.apps[index].name.clone())
}

fn ensure_interactive_terminal() -> Result<(), SelectError> {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(SelectError::NoTty)
    }
}

fn format_app_label(app: &App) -> String {
    match &app.description {
        Some(description) => format!("{:<16} {description}", app.name),
        None => app.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::format_app_label;
    use nxr_core::App;
    use std::collections::BTreeMap;

    #[test]
    fn format_app_label_includes_description_when_present() {
        let app = App {
            name: "test".to_owned(),
            attr_path: "apps.aarch64-darwin.test".to_owned(),
            flake_ref: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: Some("Run the test suite".to_owned()),
            is_default: false,
            metadata: BTreeMap::new(),
        };

        let label = format_app_label(&app);
        assert!(label.starts_with("test"));
        assert!(label.contains("Run the test suite"));
    }
}
