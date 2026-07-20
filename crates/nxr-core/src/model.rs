//! Domain models for flake apps and list output.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Opaque flake reference string (local path, `github:…`, etc.).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FlakeRef(pub String);

impl FlakeRef {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this reference into the underlying string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Local flake root as a Nix `path:` URI so Nix does not rewrite absolute
    /// paths inside a git checkout to `git+file://…?dir=…` (which rejects
    /// unlocked relative `path:../..` inputs on Nix 2.18).
    #[must_use]
    pub fn local_path(absolute: impl AsRef<str>) -> Self {
        let absolute = absolute.as_ref();
        if absolute.starts_with("path:") {
            Self(absolute.to_owned())
        } else {
            Self(format!("path:{absolute}"))
        }
    }
}

impl fmt::Display for FlakeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for FlakeRef {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for FlakeRef {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Normalized app discovered from a flake evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct App {
    pub name: String,
    pub attr_path: String,
    pub flake_ref: String,
    pub system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, JsonValue>,
}

impl App {
    #[must_use]
    pub fn to_list_app(&self) -> ListApp {
        ListApp {
            name: self.name.clone(),
            attr_path: self.attr_path.clone(),
            description: self.description.clone(),
            is_default: self.is_default,
        }
    }
}

/// Single app entry in `nxr list --json` output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListApp {
    pub name: String,
    pub attr_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "default")]
    pub is_default: bool,
}

/// Discovered flake output (package, check, or development shell).
///
/// Structurally similar to [`App`]; kept separate so app-specific metadata stays
/// on [`App`] while catalog commands share one listing shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlakeOutput {
    pub name: String,
    pub attr_path: String,
    pub flake_ref: String,
    pub system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_default: bool,
}

impl FlakeOutput {
    #[must_use]
    pub fn to_list_app(&self) -> ListApp {
        ListApp {
            name: self.name.clone(),
            attr_path: self.attr_path.clone(),
            description: self.description.clone(),
            is_default: self.is_default,
        }
    }
}

/// Versioned JSON envelope for `nxr list --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppList {
    pub schema_version: u32,
    pub flake: String,
    pub system: String,
    pub apps: Vec<ListApp>,
}

/// Versioned JSON envelope for `nxr list {packages|checks|shells} --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputList {
    pub schema_version: u32,
    pub flake: String,
    pub system: String,
    /// Catalog kind: `packages`, `checks`, or `shells`.
    pub kind: String,
    pub outputs: Vec<ListApp>,
}

impl OutputList {
    pub const SCHEMA_VERSION: u32 = 1;

    #[must_use]
    pub fn new(
        flake: impl Into<String>,
        system: impl Into<String>,
        kind: impl Into<String>,
        outputs: Vec<ListApp>,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            flake: flake.into(),
            system: system.into(),
            kind: kind.into(),
            outputs,
        }
    }

    #[must_use]
    pub fn from_outputs(
        flake: impl Into<String>,
        system: impl Into<String>,
        kind: impl Into<String>,
        outputs: impl IntoIterator<Item = FlakeOutput>,
    ) -> Self {
        Self::new(
            flake,
            system,
            kind,
            outputs
                .into_iter()
                .map(|output| output.to_list_app())
                .collect(),
        )
    }
}

impl AppList {
    pub const SCHEMA_VERSION: u32 = 1;

    #[must_use]
    pub fn new(flake: impl Into<String>, system: impl Into<String>, apps: Vec<ListApp>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            flake: flake.into(),
            system: system.into(),
            apps,
        }
    }

    #[must_use]
    pub fn from_apps(
        flake: impl Into<String>,
        system: impl Into<String>,
        apps: impl IntoIterator<Item = App>,
    ) -> Self {
        Self::new(
            flake,
            system,
            apps.into_iter().map(|app| app.to_list_app()).collect(),
        )
    }
}
