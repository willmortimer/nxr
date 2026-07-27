//! List flake inventory outputs and entries by role (output table name).

use std::collections::BTreeSet;

use crate::configurations::is_configuration_output_key;
use crate::inventory::{FlakeInventory, InventoryNode};

/// One inventory output role (flake output table name).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryRole {
    pub name: String,
    pub entry_count: usize,
}

/// One leaf entry within an inventory role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryEntry {
    pub role: String,
    pub name: String,
    pub path: Vec<String>,
    pub system: Option<String>,
    pub description: Option<String>,
    pub what: Option<String>,
}

/// List all output roles present in a parsed inventory.
#[must_use]
pub fn list_inventory_roles(
    inventory: &FlakeInventory,
    system: Option<&str>,
) -> Vec<InventoryRole> {
    let mut roles = Vec::new();
    for (name, node) in &inventory.outputs {
        let count = count_role_entries(node, system);
        if count > 0 || node.filtered {
            roles.push(InventoryRole {
                name: name.clone(),
                entry_count: count,
            });
        }
    }
    roles.sort_by(|left, right| left.name.cmp(&right.name));
    roles
}

/// List inventory entries, optionally filtered to one role.
#[must_use]
pub fn list_inventory_entries(
    inventory: &FlakeInventory,
    system: Option<&str>,
    role: Option<&str>,
) -> Vec<InventoryEntry> {
    let mut entries = Vec::new();
    for (output_name, node) in &inventory.outputs {
        if let Some(filter) = role
            && output_name != filter
        {
            continue;
        }
        if is_configuration_output_key(output_name) {
            collect_configuration_entries(output_name, node, &mut entries);
        } else {
            collect_system_entries(output_name, node, system, &mut entries);
        }
    }
    entries.sort_by(|left, right| {
        (&left.role, &left.system, &left.name).cmp(&(&right.role, &right.system, &right.name))
    });
    entries
}

fn count_role_entries(node: &InventoryNode, system: Option<&str>) -> usize {
    if node.children.is_empty() {
        return 0;
    }
    if is_configuration_output_key(node.path.first().map(String::as_str).unwrap_or("")) {
        return node
            .children
            .values()
            .filter(|child| child.is_leaf())
            .count();
    }
    let systems: BTreeSet<&str> = if let Some(system) = system {
        BTreeSet::from([system])
    } else {
        node.children.keys().map(String::as_str).collect()
    };
    let mut count = 0;
    for system_name in systems {
        if let Some(system_node) = node.children.get(system_name) {
            count += system_node
                .children
                .values()
                .filter(|child| child.is_leaf())
                .count();
        }
    }
    count
}

fn collect_configuration_entries(
    role: &str,
    node: &InventoryNode,
    entries: &mut Vec<InventoryEntry>,
) {
    for (name, leaf) in &node.children {
        if !leaf.is_leaf() {
            continue;
        }
        entries.push(InventoryEntry {
            role: role.to_owned(),
            name: name.clone(),
            path: leaf.path.clone(),
            system: None,
            description: leaf.effective_description().map(str::to_owned),
            what: leaf.what.clone(),
        });
    }
}

fn collect_system_entries(
    role: &str,
    node: &InventoryNode,
    system: Option<&str>,
    entries: &mut Vec<InventoryEntry>,
) {
    let systems: BTreeSet<&str> = if let Some(system) = system {
        BTreeSet::from([system])
    } else {
        node.children.keys().map(String::as_str).collect()
    };
    for system_name in systems {
        let Some(system_node) = node.children.get(system_name) else {
            continue;
        };
        if system_node.filtered {
            continue;
        }
        for (name, leaf) in &system_node.children {
            if !leaf.is_leaf() {
                continue;
            }
            entries.push(InventoryEntry {
                role: role.to_owned(),
                name: name.clone(),
                path: leaf.path.clone(),
                system: Some(system_name.to_owned()),
                description: leaf.effective_description().map(str::to_owned),
                what: leaf.what.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{list_inventory_entries, list_inventory_roles};
    use crate::inventory::parse_flake_inventory;

    #[test]
    fn list_roles_and_entries_for_custom_output() {
        let show = json!({
            "apps": {
                "aarch64-darwin": {
                    "hello": { "type": "app", "description": "Hello" }
                }
            },
            "customWorkflow": {
                "aarch64-darwin": {
                    "plan": { "type": "unknown", "description": "CI plan" }
                }
            }
        });
        let inventory = parse_flake_inventory(&show);
        let roles = list_inventory_roles(&inventory, Some("aarch64-darwin"));
        assert!(roles.iter().any(|role| role.name == "apps"));
        assert!(roles.iter().any(|role| role.name == "customWorkflow"));

        let entries =
            list_inventory_entries(&inventory, Some("aarch64-darwin"), Some("customWorkflow"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "plan");
        assert_eq!(entries[0].description.as_deref(), Some("CI plan"));
    }
}
