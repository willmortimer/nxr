//! Read-only discovery for `nixosConfigurations`, `darwinConfigurations`, and
//! `homeConfigurations` flake outputs.

use serde_json::Value as JsonValue;

use crate::inventory::{FlakeInventory, InventoryNode};

/// Which conventional configuration output table a flake entry belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationKind {
    /// `nixosConfigurations.<name>`
    NixOS,
    /// `darwinConfigurations.<name>`
    Darwin,
    /// `homeConfigurations.<name>`
    Home,
}

impl ConfigurationKind {
    #[must_use]
    pub const fn output_key(self) -> &'static str {
        match self {
            Self::NixOS => "nixosConfigurations",
            Self::Darwin => "darwinConfigurations",
            Self::Home => "homeConfigurations",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NixOS => "nixos",
            Self::Darwin => "darwin",
            Self::Home => "home",
        }
    }

    /// Default `nix build` attribute for this configuration kind (build only).
    #[must_use]
    pub fn build_attr_path(self, name: &str) -> String {
        match self {
            Self::NixOS => {
                format!("nixosConfigurations.{name}.config.system.build.toplevel")
            }
            Self::Darwin => format!("darwinConfigurations.{name}.system"),
            Self::Home => format!("homeConfigurations.{name}.activationPackage"),
        }
    }
}

/// One named flake configuration entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationEntry {
    pub name: String,
    pub kind: ConfigurationKind,
    pub description: Option<String>,
}

const CONFIGURATION_OUTPUTS: &[(ConfigurationKind, &str)] = &[
    (ConfigurationKind::NixOS, "nixosConfigurations"),
    (ConfigurationKind::Darwin, "darwinConfigurations"),
    (ConfigurationKind::Home, "homeConfigurations"),
];

/// List all configuration entries discovered in a parsed flake inventory.
#[must_use]
pub fn list_configurations(inventory: &FlakeInventory) -> Vec<ConfigurationEntry> {
    let mut entries = Vec::new();
    for (kind, key) in CONFIGURATION_OUTPUTS {
        entries.extend(list_configuration_output(inventory, *kind, key));
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// Resolve a configuration by name across all configuration output tables.
#[must_use]
pub fn find_configuration(inventory: &FlakeInventory, name: &str) -> Option<ConfigurationEntry> {
    list_configurations(inventory)
        .into_iter()
        .find(|entry| entry.name == name)
}

/// Build installable for a named configuration (read-only build target).
#[must_use]
pub fn configuration_installable(flake_ref: &str, entry: &ConfigurationEntry) -> String {
    format!("{flake_ref}#{}", entry.kind.build_attr_path(&entry.name))
}

/// Whether a flake show output key is a configuration table.
#[must_use]
pub fn is_configuration_output_key(name: &str) -> bool {
    matches!(
        name,
        "nixosConfigurations" | "darwinConfigurations" | "homeConfigurations"
    )
}

/// Parse a legacy `nix flake show --json` configuration output node.
#[must_use]
pub fn parse_configuration_output_node(output_name: &str, value: &JsonValue) -> InventoryNode {
    let mut output_node = InventoryNode::leaf(vec![output_name.to_owned()]);
    let Some(configs) = value.as_object() else {
        return output_node;
    };

    for (name, entry) in configs {
        let mut leaf = InventoryNode::leaf(vec![output_name.to_owned(), name.clone()]);
        leaf.legacy_type = entry
            .get("type")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        leaf.description = entry
            .get("description")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        leaf.is_legacy = true;
        output_node.children.insert(name.clone(), leaf);
    }

    output_node
}

fn list_configuration_output(
    inventory: &FlakeInventory,
    kind: ConfigurationKind,
    key: &str,
) -> Vec<ConfigurationEntry> {
    let Some(root) = inventory.outputs.get(key) else {
        return Vec::new();
    };

    root.children
        .iter()
        .filter(|(_, node)| node.is_leaf())
        .map(|(name, node)| ConfigurationEntry {
            name: name.clone(),
            kind,
            description: node.effective_description().map(str::to_owned),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ConfigurationKind, configuration_installable, find_configuration, list_configurations,
        parse_configuration_output_node,
    };
    use crate::inventory::parse_flake_inventory;

    #[test]
    fn list_configurations_from_legacy_show() {
        let show = json!({
            "nixosConfigurations": {
                "dev": {
                    "type": "nixos-configuration",
                    "description": "Dev VM"
                }
            },
            "homeConfigurations": {
                "alice": {
                    "type": "home-manager-configuration",
                    "description": "Alice laptop"
                }
            }
        });

        let inventory = parse_flake_inventory(&show);
        let entries = list_configurations(&inventory);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| {
            entry.name == "dev"
                && entry.kind == ConfigurationKind::NixOS
                && entry.description.as_deref() == Some("Dev VM")
        }));
        assert!(
            entries
                .iter()
                .any(|entry| { entry.name == "alice" && entry.kind == ConfigurationKind::Home })
        );
    }

    #[test]
    fn resolve_configuration_and_build_installable() {
        let show = json!({
            "darwinConfigurations": {
                "work": { "type": "darwin-configuration" }
            }
        });
        let inventory = parse_flake_inventory(&show);
        let entry = find_configuration(&inventory, "work").expect("work config");
        assert_eq!(entry.kind, ConfigurationKind::Darwin);
        assert_eq!(
            configuration_installable(".", &entry),
            ".#darwinConfigurations.work.system"
        );
    }

    #[test]
    fn parse_configuration_output_node_preserves_descriptions() {
        let value = json!({
            "dev": {
                "type": "nixos-configuration",
                "description": "Dev"
            }
        });
        let node = parse_configuration_output_node("nixosConfigurations", &value);
        let dev = node.children.get("dev").expect("dev");
        assert_eq!(dev.description.as_deref(), Some("Dev"));
    }
}
