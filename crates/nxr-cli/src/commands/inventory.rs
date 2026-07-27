//! `nxr inventory` — list schema-described flake inventory outputs.

use std::io::{self, Write};

use nxr_core::diagnostics::exit;
use nxr_core::sanitize::sanitize_terminal_text;
use nxr_nix::{
    InventoryEntry, InventoryRole, NixError, OptionalNixFlags, list_inventory_entries,
    list_inventory_roles, parse_flake_inventory,
};
use serde::Serialize;

use crate::commands::common::{PrepareError, build_adapter, current_invocation_directory};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while running the inventory command.
#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("inventory role not found: {role}")]
    RoleNotFound { role: String },
    #[error("inventory entry not found: {role}.{name}")]
    EntryNotFound { role: String, name: String },
}

impl InventoryError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::RoleNotFound { .. } | Self::EntryNotFound { .. } => exit::NOT_FOUND,
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

#[derive(Serialize)]
struct InventoryJson {
    schema_version: u32,
    flake: String,
    system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    roles: Vec<InventoryRoleJson>,
    entries: Vec<InventoryEntryJson>,
}

#[derive(Serialize)]
struct InventoryRoleJson {
    name: String,
    entry_count: usize,
}

#[derive(Serialize)]
struct InventoryEntryJson {
    role: String,
    name: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    what: Option<String>,
}

/// List flake inventory roles and entries.
pub fn run(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    role: Option<&str>,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), InventoryError> {
    let (flake, system, roles, entries) =
        discover_inventory(flake_arg, nix_override, role, nix_flags, runner)?;
    render_inventory(&flake, &system, role, &roles, &entries, json)
}

/// Inspect a single inventory entry.
pub fn inspect_entry(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    role: &str,
    name: &str,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), InventoryError> {
    let (flake, system, roles, entries) =
        discover_inventory(flake_arg, nix_override, Some(role), nix_flags, runner)?;
    if !roles.iter().any(|entry| entry.name == role) {
        return Err(InventoryError::RoleNotFound {
            role: role.to_owned(),
        });
    }
    let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
        return Err(InventoryError::EntryNotFound {
            role: role.to_owned(),
            name: name.to_owned(),
        });
    };
    render_entry_detail(&flake, &system, entry, json)
}

fn discover_inventory(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    role: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<
    (
        FlakeSelection,
        String,
        Vec<InventoryRole>,
        Vec<InventoryEntry>,
    ),
    InventoryError,
> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(nix_override)?;
    let system = adapter.system.clone();
    runner
        .info(format!("discovering inventory for {}", flake.display))
        .map_err(InventoryError::Io)?;

    let show = adapter.flake_show_json(&flake.nix_ref, nix_flags)?;
    let inventory = parse_flake_inventory(&show);
    let roles = list_inventory_roles(&inventory, Some(system.as_str()));
    if let Some(role) = role
        && !roles.iter().any(|entry| entry.name == role)
    {
        return Err(InventoryError::RoleNotFound {
            role: role.to_owned(),
        });
    }
    let entries = list_inventory_entries(&inventory, Some(system.as_str()), role);
    Ok((flake, system, roles, entries))
}

fn render_inventory(
    flake: &FlakeSelection,
    system: &str,
    role: Option<&str>,
    roles: &[InventoryRole],
    entries: &[InventoryEntry],
    json: bool,
) -> Result<(), InventoryError> {
    let mut stdout = io::stdout().lock();
    if json {
        let payload = InventoryJson {
            schema_version: 1,
            flake: flake.display.clone(),
            system: system.to_owned(),
            role: role.map(str::to_owned),
            roles: roles
                .iter()
                .map(|entry| InventoryRoleJson {
                    name: entry.name.clone(),
                    entry_count: entry.entry_count,
                })
                .collect(),
            entries: entries.iter().map(entry_to_json).collect(),
        };
        writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)?;
        return Ok(());
    }

    if roles.is_empty() {
        writeln!(stdout, "No inventory roles found for {}", flake.display)?;
        return Ok(());
    }

    writeln!(stdout, "Inventory roles for {} ({system}):", flake.display)?;
    for role_entry in roles {
        writeln!(
            stdout,
            "  {} ({} entries)",
            role_entry.name, role_entry.entry_count
        )?;
    }

    if let Some(role) = role {
        writeln!(stdout)?;
        writeln!(stdout, "Entries for role `{role}`:")?;
        if entries.is_empty() {
            writeln!(stdout, "  (none)")?;
        } else {
            for entry in entries {
                render_entry(&mut stdout, entry)?;
            }
        }
    }

    Ok(())
}

fn render_entry_detail(
    flake: &FlakeSelection,
    system: &str,
    entry: &InventoryEntry,
    json: bool,
) -> Result<(), InventoryError> {
    let mut stdout = io::stdout().lock();
    if json {
        let payload = entry_to_json(entry);
        writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)?;
        return Ok(());
    }
    writeln!(stdout, "inventory: {}.{}", entry.role, entry.name)?;
    writeln!(stdout, "flake: {}", flake.display)?;
    writeln!(stdout, "system: {system}")?;
    writeln!(stdout, "path: {}", entry.path.join("."))?;
    if let Some(description) = entry.description.as_deref() {
        writeln!(
            stdout,
            "description: {}",
            sanitize_terminal_text(description)
        )?;
    }
    if let Some(what) = entry.what.as_deref() {
        writeln!(stdout, "what: {what}")?;
    }
    Ok(())
}

fn entry_to_json(entry: &InventoryEntry) -> InventoryEntryJson {
    InventoryEntryJson {
        role: entry.role.clone(),
        name: entry.name.clone(),
        path: entry.path.join("."),
        system: entry.system.clone(),
        description: entry.description.clone(),
        what: entry.what.clone(),
    }
}

fn render_entry(stdout: &mut impl Write, entry: &InventoryEntry) -> io::Result<()> {
    let system = entry
        .system
        .as_deref()
        .map(|value| format!(" ({value})"))
        .unwrap_or_default();
    let description = entry
        .description
        .as_deref()
        .map(|text| format!(" — {}", sanitize_terminal_text(text)))
        .unwrap_or_default();
    writeln!(stdout, "  {}{system}{description}", entry.name)
}
