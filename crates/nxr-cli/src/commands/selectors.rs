//! CLI wiring for task selectors.

use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_nix::{NixError, OptionalNixFlags, TaskDiscoveryError};
use nxr_task::{
    APP_PREFIX, CATEGORY_PREFIX, CHANGED_SELECTOR, SelectorError, TASK_PREFIX, TaskDocument,
    TaskTargetResolution, resolve_task_targets,
};

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};

/// Errors while expanding selectors at the CLI layer.
#[derive(Debug, thiserror::Error)]
pub enum SelectorCommandError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    Selector(#[from] SelectorError),
    #[error("{0}")]
    Usage(String),
}

impl SelectorCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Tasks(error) => error.exit_code(),
            Self::Selector(_) | Self::Usage(_) => nxr_core::diagnostics::exit::USAGE,
        }
    }
}

/// Whether a token uses selector syntax (`category:`, `changed`, `task:`, …).
#[must_use]
pub fn token_is_selector(token: &str) -> bool {
    token == CHANGED_SELECTOR
        || token.starts_with(CATEGORY_PREFIX)
        || token.starts_with(TASK_PREFIX)
        || token.starts_with(APP_PREFIX)
}

/// Whether any token requests affected-style selection (`changed`).
#[must_use]
pub fn tokens_request_affected(tokens: &[String]) -> bool {
    tokens.iter().any(|token| token == CHANGED_SELECTOR)
}

/// Tokens with the `changed` selector removed (for task-name expansion).
#[must_use]
pub fn tokens_without_changed(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .filter(|token| *token != CHANGED_SELECTOR)
        .cloned()
        .collect()
}

/// Load the flake task document for selector expansion.
pub fn load_task_document(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, SelectorCommandError> {
    discover_task_document(flake_arg, nix_override, refresh_discovery, nix_flags)
}

/// Expand selector tokens against the flake task document.
///
/// # Errors
///
/// Returns [`SelectorCommandError`] when discovery or expansion fails.
pub fn expand_task_tokens(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
    tokens: &[String],
) -> Result<TaskTargetResolution, SelectorCommandError> {
    let document = discover_task_document(flake_arg, nix_override, refresh_discovery, nix_flags)?;
    Ok(resolve_task_targets(
        &document,
        &tokens_without_changed(tokens),
    )?)
}

fn discover_task_document(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, SelectorCommandError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;
    discover_task_document_for_flake(&flake, &adapter, refresh_discovery, nix_flags)
}

fn discover_task_document_for_flake(
    flake: &FlakeSelection,
    adapter: &nxr_nix::NixAdapter,
    refresh_discovery: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, SelectorCommandError> {
    let context = DiscoveryContext {
        flake_ref: flake.nix_ref.clone(),
        local_root: flake.local_root.clone(),
        system: adapter.system.clone(),
        nix_path: adapter.nix.as_str().to_owned(),
        nix_version: adapter.capabilities.version.to_string(),
        discovery_inputs: Vec::new(),
    };
    let flake_ref = flake.nix_ref.clone();
    let workspace = discover_workspace_with_cache(
        &context,
        DiscoveryCacheOptions::with_tasks(refresh_discovery),
        || -> Result<WorkspaceDiscovery, SelectorCommandError> {
            let apps = adapter
                .discover_apps(&flake_ref, nix_flags)
                .map_err(SelectorCommandError::Nix)?;
            let tasks = adapter
                .discover_tasks(&flake_ref, nix_flags)
                .map_err(SelectorCommandError::Tasks)?;
            Ok(WorkspaceDiscovery {
                apps,
                tasks: Some(tasks),
            })
        },
    )?;
    workspace
        .tasks
        .ok_or_else(|| SelectorCommandError::Usage("flake has no nxr tasks".to_owned()))
}
