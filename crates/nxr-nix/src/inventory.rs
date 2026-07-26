//! Generic flake inventory AST and adapters for legacy and Determinate v2 shapes.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

/// Which flake output table to parse from `nix flake show --json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTable {
    /// `apps.<system>.*` (`type == "app"`).
    Apps,
    /// `packages.<system>.*` (`type == "derivation"`).
    Packages,
    /// `checks.<system>.*` (`type == "derivation"`).
    Checks,
    /// `devShells.<system>.*` (`type == "derivation"`).
    DevShells,
}

impl OutputTable {
    #[must_use]
    pub const fn show_key(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::Packages => "packages",
            Self::Checks => "checks",
            Self::DevShells => "devShells",
        }
    }

    #[must_use]
    pub const fn attr_prefix(self) -> &'static str {
        match self {
            Self::Apps => "apps",
            Self::Packages => "packages",
            Self::Checks => "checks",
            Self::DevShells => "devShells",
        }
    }

    /// Upstream `nix flake show --json` `type` field (pre-inventory format).
    #[must_use]
    pub const fn expected_type(self) -> &'static str {
        match self {
            Self::Apps => "app",
            Self::Packages | Self::Checks | Self::DevShells => "derivation",
        }
    }

    /// Determinate Nix inventory v2 `what` field (flake-schemas), used as a hint.
    #[must_use]
    pub const fn expected_what(self) -> &'static str {
        match self {
            Self::Apps => "app",
            Self::Packages => "package",
            Self::Checks => "CI test",
            Self::DevShells => "development environment",
        }
    }
}

/// Normalized kind for standard flake output tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardOutputKind {
    App,
    Package,
    Check,
    DevShell,
}

impl From<OutputTable> for StandardOutputKind {
    fn from(table: OutputTable) -> Self {
        match table {
            OutputTable::Apps => Self::App,
            OutputTable::Packages => Self::Package,
            OutputTable::Checks => Self::Check,
            OutputTable::DevShells => Self::DevShell,
        }
    }
}

/// Parsed flake show / inventory document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlakeInventory {
    pub version: Option<u32>,
    pub outputs: BTreeMap<String, InventoryNode>,
}

/// One node in the inventory tree (output root, system, or leaf entry).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryNode {
    pub path: Vec<String>,
    pub children: BTreeMap<String, InventoryNode>,
    pub for_systems: Vec<String>,
    pub what: Option<String>,
    pub short_description: Option<String>,
    pub legacy_type: Option<String>,
    pub description: Option<String>,
    pub derivation_attr_path: Option<Vec<String>>,
    pub eval_checks: BTreeMap<String, bool>,
    pub is_flake_check: bool,
    pub is_legacy: bool,
    pub filtered: bool,
}

impl InventoryNode {
    #[must_use]
    pub fn leaf(path: Vec<String>) -> Self {
        Self {
            path,
            children: BTreeMap::new(),
            for_systems: Vec::new(),
            what: None,
            short_description: None,
            legacy_type: None,
            description: None,
            derivation_attr_path: None,
            eval_checks: BTreeMap::new(),
            is_flake_check: false,
            is_legacy: false,
            filtered: false,
        }
    }

    #[must_use]
    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    #[must_use]
    pub fn effective_description(&self) -> Option<&str> {
        self.short_description
            .as_deref()
            .or(self.description.as_deref())
            .filter(|text| !text.is_empty())
    }
}

/// Parse any supported `nix flake show --json` envelope into a generic inventory AST.
///
/// Legacy top-level output tables take precedence over inventory v2 entries with the
/// same name, matching historical discovery behavior.
#[must_use]
pub fn parse_flake_inventory(show: &JsonValue) -> FlakeInventory {
    let mut inventory = parse_legacy_show(show);

    if show.get("inventory").is_some() {
        let v2 = parse_inventory_v2(show);
        for (name, node) in v2.outputs {
            inventory.outputs.entry(name).or_insert(node);
        }
        inventory.version = v2.version;
    }

    inventory
}

/// List leaf entries for a standard output table and system from a parsed inventory.
#[must_use]
pub fn list_standard_outputs(
    inventory: &FlakeInventory,
    table: OutputTable,
    system: &str,
) -> Vec<StandardOutputEntry> {
    let Some(output_root) = inventory.outputs.get(table.show_key()) else {
        return Vec::new();
    };

    let Some(system_node) = output_root.children.get(system) else {
        return Vec::new();
    };

    if system_node.filtered {
        return Vec::new();
    }

    let kind = StandardOutputKind::from(table);
    let mut entries = Vec::new();
    for (name, node) in &system_node.children {
        if !node.is_leaf() {
            continue;
        }
        if !node_matches_kind(node, kind, table) {
            continue;
        }
        entries.push(StandardOutputEntry {
            name: name.clone(),
            description: node.effective_description().map(str::to_owned),
            is_default: name == "default",
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

/// Leaf entry extracted from a standard output table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardOutputEntry {
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
}

fn parse_legacy_show(show: &JsonValue) -> FlakeInventory {
    let Some(root) = show.as_object() else {
        return FlakeInventory {
            version: None,
            outputs: BTreeMap::new(),
        };
    };

    let mut outputs = BTreeMap::new();
    for (output_name, output_value) in root {
        if let Some(node) = parse_legacy_output_node(output_name, output_value) {
            outputs.insert(output_name.clone(), node);
        }
    }

    FlakeInventory {
        version: None,
        outputs,
    }
}

fn parse_legacy_output_node(output_name: &str, value: &JsonValue) -> Option<InventoryNode> {
    let systems = value.as_object()?;
    let mut output_node = InventoryNode::leaf(vec![output_name.to_owned()]);

    for (system, system_value) in systems {
        let entries = system_value.as_object()?;
        let mut system_node = InventoryNode::leaf(vec![output_name.to_owned(), system.clone()]);
        system_node.for_systems = vec![system.clone()];

        for (name, entry) in entries {
            let mut leaf =
                InventoryNode::leaf(vec![output_name.to_owned(), system.clone(), name.clone()]);
            leaf.legacy_type = entry
                .get("type")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            leaf.description = entry
                .get("description")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            leaf.is_legacy = true;
            absorb_additive_fields(&mut leaf, entry);
            system_node.children.insert(name.clone(), leaf);
        }

        output_node.children.insert(system.clone(), system_node);
    }

    Some(output_node)
}

fn parse_inventory_v2(show: &JsonValue) -> FlakeInventory {
    let version = show
        .get("version")
        .and_then(JsonValue::as_u64)
        .map(|v| v as u32);
    let inventory = show
        .get("inventory")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();

    let mut outputs = BTreeMap::new();
    for (output_name, output_value) in inventory {
        if let Some(node) = parse_inventory_v2_output_node(&output_name, &output_value) {
            outputs.insert(output_name, node);
        }
    }

    FlakeInventory { version, outputs }
}

fn parse_inventory_v2_output_node(output_name: &str, value: &JsonValue) -> Option<InventoryNode> {
    let output_children = value
        .get("output")
        .and_then(|output| output.get("children"))
        .and_then(JsonValue::as_object)?;

    let mut output_node = InventoryNode::leaf(vec![output_name.to_owned()]);
    absorb_additive_fields(&mut output_node, value);

    for (system, system_value) in output_children {
        let mut system_node = InventoryNode::leaf(vec![output_name.to_owned(), system.clone()]);
        system_node.for_systems = string_array(system_value.get("forSystems"));
        system_node.filtered = system_value
            .get("filtered")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        absorb_additive_fields(&mut system_node, system_value);

        if let Some(entries) = system_value.get("children").and_then(JsonValue::as_object) {
            for (name, entry) in entries {
                let mut leaf =
                    InventoryNode::leaf(vec![output_name.to_owned(), system.clone(), name.clone()]);
                leaf.what = entry
                    .get("what")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned);
                leaf.short_description = entry
                    .get("shortDescription")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned);
                leaf.for_systems = string_array(entry.get("forSystems"));
                leaf.derivation_attr_path = string_array_opt(
                    entry
                        .get("derivationAttrPath")
                        .or_else(|| entry.get("derivation_attr_path")),
                );
                leaf.is_flake_check = entry
                    .get("isFlakeCheck")
                    .or_else(|| entry.get("is_flake_check"))
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false);
                if let Some(checks) = entry.get("evalChecks").and_then(JsonValue::as_object) {
                    for (key, value) in checks {
                        if let Some(flag) = value.as_bool() {
                            leaf.eval_checks.insert(key.clone(), flag);
                        }
                    }
                }
                absorb_additive_fields(&mut leaf, entry);
                system_node.children.insert(name.clone(), leaf);
            }
        }

        output_node.children.insert(system.clone(), system_node);
    }

    Some(output_node)
}

/// Tolerate additive inventory fields by ignoring unknown keys during parse.
fn absorb_additive_fields(_node: &mut InventoryNode, _value: &JsonValue) {}

fn string_array(value: Option<&JsonValue>) -> Vec<String> {
    value
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn string_array_opt(value: Option<&JsonValue>) -> Option<Vec<String>> {
    let array = string_array(value);
    if array.is_empty() { None } else { Some(array) }
}

fn node_matches_kind(node: &InventoryNode, kind: StandardOutputKind, table: OutputTable) -> bool {
    if node.is_legacy {
        return node
            .legacy_type
            .as_deref()
            .is_some_and(|legacy_type| legacy_type == table.expected_type());
    }

    match node.what.as_deref().map(what_kind) {
        None => true,
        Some(Some(node_kind)) => node_kind == kind,
        Some(None) => true,
    }
}

fn what_kind(what: &str) -> Option<StandardOutputKind> {
    match what.trim().to_ascii_lowercase().as_str() {
        "app" => Some(StandardOutputKind::App),
        "package" => Some(StandardOutputKind::Package),
        "ci test" | "check" => Some(StandardOutputKind::Check),
        "development environment" | "dev shell" | "development shell" => {
            Some(StandardOutputKind::DevShell)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::OutputTable;
    use super::{
        StandardOutputKind, list_standard_outputs, parse_flake_inventory, what_kind,
    };

    #[test]
    fn parse_preserves_unknown_output_tables() {
        let show = json!({
            "apps": {
                "aarch64-darwin": {
                    "hello": { "type": "app" }
                }
            },
            "customWorkflow": {
                "aarch64-darwin": {
                    "plan": { "type": "unknown", "description": "CI plan" }
                }
            }
        });

        let inventory = parse_flake_inventory(&show);
        assert_eq!(inventory.version, None);
        assert!(inventory.outputs.contains_key("apps"));
        assert!(inventory.outputs.contains_key("customWorkflow"));

        let custom = inventory
            .outputs
            .get("customWorkflow")
            .expect("custom output");
        let system = custom.children.get("aarch64-darwin").expect("system node");
        let plan = system.children.get("plan").expect("plan leaf");
        assert_eq!(plan.legacy_type.as_deref(), Some("unknown"));
        assert_eq!(plan.description.as_deref(), Some("CI plan"));
    }

    #[test]
    fn parse_inventory_v2_preserves_unknown_outputs_and_additive_fields() {
        let show = json!({
            "version": 2,
            "inventory": {
                "apps": {
                    "doc": "The apps output",
                    "futureField": "ignored-but-tolerated",
                    "output": {
                        "children": {
                            "x86_64-linux": {
                                "forSystems": ["x86_64-linux"],
                                "children": {
                                    "hello": {
                                        "what": "app",
                                        "shortDescription": "Greet",
                                        "vendorExtension": { "tier": 1 }
                                    }
                                }
                            }
                        }
                    }
                },
                "nxr": {
                    "doc": "NXR tasks",
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "test": {
                                        "what": "NXR task",
                                        "shortDescription": "Run tests"
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "extraTopLevel": true
        });

        let inventory = parse_flake_inventory(&show);
        assert_eq!(inventory.version, Some(2));
        assert!(inventory.outputs.contains_key("apps"));
        assert!(inventory.outputs.contains_key("nxr"));

        let nxr = inventory.outputs.get("nxr").expect("nxr output");
        let task = nxr
            .children
            .get("aarch64-darwin")
            .and_then(|system| system.children.get("test"))
            .expect("task leaf");
        assert_eq!(task.what.as_deref(), Some("NXR task"));
        assert_eq!(task.short_description.as_deref(), Some("Run tests"));
    }

    #[test]
    fn list_apps_by_path_without_what_field() {
        let show = json!({
            "version": 2,
            "inventory": {
                "apps": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "implicit-app": {
                                        "shortDescription": "Path-selected app"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let inventory = parse_flake_inventory(&show);
        let apps = list_standard_outputs(&inventory, OutputTable::Apps, "aarch64-darwin");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "implicit-app");
        assert_eq!(apps[0].description.as_deref(), Some("Path-selected app"));
    }

    #[test]
    fn list_excludes_conflicting_what_under_known_path() {
        let show = json!({
            "version": 2,
            "inventory": {
                "apps": {
                    "output": {
                        "children": {
                            "aarch64-darwin": {
                                "children": {
                                    "wrong-kind": { "what": "package" },
                                    "right-kind": { "what": "app" }
                                }
                            }
                        }
                    }
                }
            }
        });

        let inventory = parse_flake_inventory(&show);
        let apps = list_standard_outputs(&inventory, OutputTable::Apps, "aarch64-darwin");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "right-kind");
    }

    #[test]
    fn what_kind_normalizes_common_variants() {
        assert_eq!(what_kind("app"), Some(StandardOutputKind::App));
        assert_eq!(what_kind("package"), Some(StandardOutputKind::Package));
        assert_eq!(what_kind("CI test"), Some(StandardOutputKind::Check));
        assert_eq!(
            what_kind("development environment"),
            Some(StandardOutputKind::DevShell)
        );
        assert_eq!(what_kind("NXR task"), None);
    }
}
