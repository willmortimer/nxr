//! Project identity and runtime secret preparation for task/context execution.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use nxr_core::config::{SecretBindings, UserConfig, load_secret_bindings, load_user_config};
use nxr_task::{
    ContextError, PlanSecretEntry, ResolvedSecrets, SecretProvider, authorize_secret_refs,
    merge_spawn_env_overrides, resolve_context_secrets, secret_refs_for_entries,
};

/// Prepared spawn-time secret material (keep alive until the child exits).
pub struct SpawnSecrets {
    pub env_overrides: BTreeMap<String, String>,
    pub stdin_payload: Option<Vec<u8>>,
    #[allow(dead_code)]
    guards: ResolvedSecrets, // keeps tempfiles alive until drop
}

impl SpawnSecrets {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            env_overrides: BTreeMap::new(),
            stdin_payload: None,
            guards: ResolvedSecrets {
                env_overrides: BTreeMap::new(),
                temp_files: Vec::new(),
                stdin_payload: None,
            },
        }
    }
}

/// Load user config and secret bindings from the standard config directory.
///
/// # Errors
///
/// Returns [`ContextError::Config`] when configuration files cannot be read or parsed.
pub fn load_runtime_secret_config() -> Result<(UserConfig, SecretBindings), ContextError> {
    let user = load_user_config().map_err(|error| ContextError::Config {
        message: error.to_string(),
    })?;
    let bindings = load_secret_bindings().map_err(|error| ContextError::Config {
        message: error.to_string(),
    })?;
    Ok((user, bindings))
}

/// Canonical project identity for trust checks (`github.com/org/repo`, or `local`).
#[must_use]
pub fn project_identity(flake_root: &Path) -> String {
    let output = Command::new("git")
        .args([
            "-C",
            &flake_root.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ])
        .output();
    let Ok(output) = output else {
        return "local".to_owned();
    };
    if !output.status.success() {
        return "local".to_owned();
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    normalize_remote_url(&url).unwrap_or_else(|| "local".to_owned())
}

fn normalize_remote_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return Some(
            format!("github.com/{rest}")
                .trim_end_matches(".git")
                .to_owned(),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        return Some(
            format!("github.com/{rest}")
                .trim_end_matches(".git")
                .to_owned(),
        );
    }
    Some(trimmed.trim_end_matches(".git").to_owned())
}

/// Resolve and authorize secrets for a single spawn.
///
/// # Errors
///
/// Returns [`ContextError`] when authorization or resolution fails.
pub fn prepare_spawn_secrets(
    entries: &[PlanSecretEntry],
    project_id: &str,
    user_config: &UserConfig,
    bindings: &SecretBindings,
) -> Result<SpawnSecrets, ContextError> {
    if entries.is_empty() {
        return Ok(empty_spawn_secrets());
    }

    let trust_refs = refs_requiring_trust(entries);
    if !trust_refs.is_empty() && !user_config.trusted_projects.is_empty() {
        authorize_secret_refs(project_id, &trust_refs, &user_config.trusted_projects)?;
    }

    let guards = resolve_context_secrets(entries, bindings, |name| std::env::var(name).ok())?;
    let env_overrides = merge_spawn_env_overrides(&BTreeMap::new(), &guards.env_overrides);
    Ok(SpawnSecrets {
        env_overrides,
        stdin_payload: guards.stdin_payload.clone(),
        guards,
    })
}

fn empty_spawn_secrets() -> SpawnSecrets {
    SpawnSecrets::empty()
}

pub fn refs_requiring_trust(entries: &[PlanSecretEntry]) -> Vec<String> {
    if entries
        .iter()
        .all(|entry| entry.provider == SecretProvider::Env)
    {
        return Vec::new();
    }
    secret_refs_for_entries(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_github_ssh_remote() {
        assert_eq!(
            normalize_remote_url("git@github.com:org/repo.git").as_deref(),
            Some("github.com/org/repo")
        );
    }

    #[test]
    fn normalize_github_https_remote() {
        assert_eq!(
            normalize_remote_url("https://github.com/org/repo").as_deref(),
            Some("github.com/org/repo")
        );
    }
}
