//! Read-only configuration adapters (`list` / `inspect` / `build`).

use std::fmt;
use std::io::{self, Write};

use nxr_core::diagnostics::exit;
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{
    ConfigurationEntry, NixError, OptionalNixFlags, configuration_installable, list_configurations,
    parse_flake_inventory,
};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while listing or inspecting configurations.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    NotFound(#[from] ConfigurationNotFoundError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl ConfigurationError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::NotFound(_) => ConfigurationNotFoundError::exit_code(),
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

/// Named configuration not found in flake inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationNotFoundError {
    pub name: String,
    pub suggestions: Vec<String>,
}

impl fmt::Display for ConfigurationNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "configuration not found: {}", self.name)?;
        if self.suggestions.is_empty() {
            return Ok(());
        }
        writeln!(f)?;
        writeln!(f)?;
        writeln!(f, "Did you mean:")?;
        for suggestion in &self.suggestions {
            writeln!(f, "  {suggestion}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigurationNotFoundError {}

impl ConfigurationNotFoundError {
    #[must_use]
    pub const fn exit_code() -> i32 {
        exit::NOT_FOUND
    }
}

#[derive(Serialize)]
struct ConfigurationListJson {
    schema_version: u32,
    flake: String,
    configurations: Vec<ConfigurationListEntryJson>,
}

#[derive(Serialize)]
struct ConfigurationListEntryJson {
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    build_attr: String,
}

#[derive(Serialize)]
struct ConfigurationInspectJson {
    schema_version: u32,
    flake: String,
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    build_attr: String,
    installable: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    devenv: Option<String>,
}

fn discover_configurations(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
) -> Result<(FlakeSelection, Vec<ConfigurationEntry>), ConfigurationError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;
    let show = adapter.flake_show_json(&flake.nix_ref, nix_flags)?;
    let inventory = parse_flake_inventory(&show);
    Ok((flake, list_configurations(&inventory)))
}

fn configuration_not_found(
    name: &str,
    entries: &[ConfigurationEntry],
) -> ConfigurationNotFoundError {
    let known: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let suggestions = nxr_nix::rank_name_suggestions(name, known, 3)
        .into_iter()
        .map(str::to_owned)
        .collect();
    ConfigurationNotFoundError {
        name: name.to_owned(),
        suggestions,
    }
}

/// List discovered flake configurations.
///
/// # Errors
///
/// Returns [`ConfigurationError`] when discovery or output fails.
pub fn list(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), ConfigurationError> {
    let (flake, entries) = discover_configurations(flake_arg, nix_override, nix_flags)?;
    runner
        .info(format!("discovering configurations for {}", flake.display))
        .map_err(ConfigurationError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        let payload = ConfigurationListJson {
            schema_version: 1,
            flake: flake.display.clone(),
            configurations: entries
                .iter()
                .map(|entry| ConfigurationListEntryJson {
                    name: entry.name.clone(),
                    kind: entry.kind.label().to_owned(),
                    description: entry.description.clone(),
                    build_attr: entry.kind.build_attr_path(&entry.name),
                })
                .collect(),
        };
        writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)?;
    } else if entries.is_empty() {
        writeln!(stdout, "No configurations found for {}", flake.display)?;
    } else {
        writeln!(stdout, "Available configurations for {}:", flake.display)?;
        for entry in &entries {
            let description = entry
                .description
                .as_deref()
                .map(|text| format!("  {text}"))
                .unwrap_or_default();
            writeln!(
                stdout,
                "  {} ({}){}",
                entry.name,
                entry.kind.label(),
                description
            )?;
        }
    }

    Ok(())
}

/// Inspect a single flake configuration (read-only metadata).
///
/// # Errors
///
/// Returns [`ConfigurationError`] when discovery or output fails.
pub fn inspect(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    name: &str,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), ConfigurationError> {
    let (flake, entries) = discover_configurations(flake_arg, nix_override, nix_flags)?;
    let entry = find_configuration_entry(&entries, name)?;
    let installable = configuration_installable(&flake.nix_ref, entry);
    let devenv = detect_devenv_hint(flake.local_root.as_deref());

    runner
        .info(format!("inspecting configuration {name}"))
        .map_err(ConfigurationError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        let payload = ConfigurationInspectJson {
            schema_version: 1,
            flake: flake.display.clone(),
            name: entry.name.clone(),
            kind: entry.kind.label().to_owned(),
            description: entry.description.clone(),
            build_attr: entry.kind.build_attr_path(&entry.name),
            installable,
            devenv,
        };
        writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)?;
    } else {
        writeln!(stdout, "configuration: {}", entry.name)?;
        writeln!(stdout, "flake: {}", flake.display)?;
        writeln!(stdout, "kind: {}", entry.kind.label())?;
        if let Some(description) = entry.description.as_deref() {
            writeln!(
                stdout,
                "description: {}",
                sanitize_terminal_text(description)
            )?;
        }
        writeln!(
            stdout,
            "build attr: {}",
            entry.kind.build_attr_path(&entry.name)
        )?;
        writeln!(stdout, "installable: {installable}")?;
        if let Some(hint) = devenv {
            writeln!(stdout, "devenv: {hint}")?;
        }
    }

    Ok(())
}

/// Resolve a configuration entry for `nxr build configuration <name>`.
///
/// # Errors
///
/// Returns [`ConfigurationError`] when the configuration cannot be found.
pub fn resolve_for_build(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    name: &str,
    nix_flags: &OptionalNixFlags,
) -> Result<(FlakeSelection, ConfigurationEntry, String), ConfigurationError> {
    let (flake, entries) = discover_configurations(flake_arg, nix_override, nix_flags)?;
    let entry = find_configuration_entry(&entries, name)?;
    let installable = configuration_installable(&flake.nix_ref, entry);
    Ok((flake, entry.clone(), installable))
}

fn find_configuration_entry<'a>(
    entries: &'a [ConfigurationEntry],
    name: &str,
) -> Result<&'a ConfigurationEntry, ConfigurationError> {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| configuration_not_found(name, entries).into())
}

fn detect_devenv_hint(local_root: Option<&camino::Utf8Path>) -> Option<String> {
    let root = local_root?;
    if root.join("devenv.yaml").is_file() {
        return Some("devenv.yaml present (read-only; nxr does not activate devenv)".to_owned());
    }
    if root.join("devenv.nix").is_file() {
        return Some("devenv.nix present (read-only; nxr does not activate devenv)".to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::ConfigurationNotFoundError;

    #[test]
    fn configuration_not_found_formats_suggestions() {
        let error = ConfigurationNotFoundError {
            name: "devl".to_owned(),
            suggestions: vec!["dev".to_owned()],
        };
        let rendered = error.to_string();
        assert!(rendered.contains("configuration not found: devl"));
        assert!(rendered.contains("dev"));
    }
}
