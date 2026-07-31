//! `nxr ui` — lazygit-style browser over apps, tasks, and workspace scripts.

use std::io::{self, IsTerminal};

use nxr_core::App;
use nxr_core::diagnostics::exit;
use nxr_nix::OptionalNixFlags;
use nxr_task::TaskDefinition;

use crate::commands::list::{self, CatalogEntries};
use crate::commands::script::{self, ConventionScriptEntry};
use crate::runner_output::RunnerOutput;
use crate::tui::browser::{BrowserLaunch, BrowserOutcome, BrowserState, run_browser};

/// Errors from `nxr ui`.
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error(transparent)]
    List(#[from] list::ListError),
    #[error(transparent)]
    Script(#[from] script::ScriptError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("ui requires an interactive terminal")]
    NotTty,
}

impl UiError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::List(error) => error.exit_code(),
            Self::Script(error) => error.exit_code(),
            Self::NotTty => exit::USAGE,
            Self::Io(_) => exit::EVALUATION,
        }
    }
}

/// Discover apps, tasks, and convention scripts for the browser.
///
/// # Errors
///
/// Returns [`UiError`] when discovery fails.
pub fn discover_catalog(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(CatalogEntries, Vec<ConventionScriptEntry>), UiError> {
    let catalog = list::discover_catalog(
        flake_arg,
        nix_override,
        refresh_discovery,
        nix_flags,
        runner,
    )?;
    let scripts = script::list_convention_scripts(flake_arg, nix_override)?;
    Ok((catalog, scripts))
}

/// Open the browser and return a launch target when the user presses Enter.
///
/// # Errors
///
/// Returns [`UiError`] on non-TTY hosts, discovery failures, or terminal I/O errors.
pub fn run_interactive(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<Option<BrowserLaunch>, UiError> {
    ensure_tty()?;

    let (catalog, scripts) = discover_catalog(
        flake_arg,
        nix_override,
        refresh_discovery,
        nix_flags,
        runner,
    )?;

    let state = browser_state_from_catalog(&catalog, &scripts);
    match run_browser(state)? {
        BrowserOutcome::Quit => Ok(None),
        BrowserOutcome::Launch(launch) => Ok(Some(launch)),
    }
}

fn ensure_tty() -> Result<(), UiError> {
    if io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(UiError::NotTty)
    }
}

fn browser_state_from_catalog(
    catalog: &CatalogEntries,
    scripts: &[ConventionScriptEntry],
) -> BrowserState {
    BrowserState::from_catalog(
        catalog.apps.iter().map(app_row),
        catalog.tasks.iter().map(task_row),
        scripts
            .iter()
            .map(|entry| (entry.name.clone(), Some(entry.path.clone()))),
    )
}

fn app_row(app: &App) -> (String, Option<String>) {
    (app.name.clone(), app.description.clone())
}

fn task_row((name, task): (&String, &TaskDefinition)) -> (String, Option<String>) {
    (name.clone(), task.description.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_core::App;
    use nxr_task::TaskDefinition;
    use std::collections::BTreeMap;

    #[test]
    fn non_tty_stderr_is_usage_error() {
        // Piped stderr in unit tests is never a TTY.
        let err = ensure_tty().expect_err("expected non-tty failure");
        assert!(matches!(err, UiError::NotTty));
        assert_eq!(err.exit_code(), exit::USAGE);
    }

    #[test]
    fn browser_state_maps_catalog_rows() {
        let catalog = CatalogEntries {
            apps: vec![App {
                name: "fmt".to_owned(),
                attr_path: "apps.system.fmt".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("format".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            }],
            tasks: BTreeMap::from([(
                "ci".to_owned(),
                TaskDefinition {
                    description: Some("run ci".to_owned()),
                    app: "ci".to_owned(),
                    depends_on: Vec::new(),
                    working_directory: None,
                    hidden: false,
                    category: None,
                    aliases: Vec::new(),
                    interactive: false,
                    paths: Vec::new(),
                    timeout: None,
                    termination_grace_period: None,
                    inputs: None,
                    outputs: Vec::new(),
                    cache: None,
                    resources: None,
                    shell: None,
                    context: None,
                    parameters: BTreeMap::new(),
                    matrix: None,
                },
            )]),
        };
        let scripts = vec![ConventionScriptEntry {
            name: "lint".to_owned(),
            path: ".nxr/scripts/lint".to_owned(),
        }];
        let state = browser_state_from_catalog(&catalog, &scripts);
        assert_eq!(state.apps.len(), 1);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.scripts.len(), 1);
        assert_eq!(state.apps[0].name, "fmt");
        assert_eq!(state.tasks[0].name, "ci");
        assert_eq!(state.scripts[0].name, "lint");
    }
}
