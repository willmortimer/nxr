//! Versioned task schema types and validation helpers.
//!
//! The envelope is `schema_version` plus a map of task name → [`TaskDefinition`].
//! Field names in JSON match the flake metadata vocabulary (`dependsOn`,
//! `workingDirectory`). Unknown task fields are tolerated by serde defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Supported major version for the task schema envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// Errors produced while validating a task schema document.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SchemaError {
    /// Document major version is not supported by this crate.
    #[error("unsupported task schema version {found}; expected major version {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
}

/// Versioned task document: `schema_version` plus named task definitions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskDefinition>,
}

impl TaskDocument {
    /// Supported major schema version for this document type.
    pub const SCHEMA_VERSION: u32 = SCHEMA_VERSION;

    /// Create a document with the current schema version.
    #[must_use]
    pub fn new(tasks: BTreeMap<String, TaskDefinition>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            tasks,
        }
    }

    /// Validate that [`Self::schema_version`] is supported.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::UnsupportedVersion`] when the major version is not
    /// [`SCHEMA_VERSION`].
    pub fn validate(&self) -> Result<(), SchemaError> {
        validate_schema_version(self.schema_version)
    }
}

/// Single task definition (MVP fields).
///
/// `app` is required and names the flake app leaf this task runs. Optional
/// fields mirror the flake-parts / Nix metadata vocabulary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Flake app leaf name (`apps.<system>.<name>`).
    pub app: String,

    #[serde(
        default,
        rename = "workingDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub working_directory: Option<String>,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl TaskDefinition {
    /// Create a minimal task that runs `app` with no dependencies.
    #[must_use]
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            description: None,
            depends_on: Vec::new(),
            app: app.into(),
            working_directory: None,
            hidden: false,
            category: None,
        }
    }
}

/// Reject unsupported major schema versions.
///
/// # Errors
///
/// Returns [`SchemaError::UnsupportedVersion`] when `version` is not
/// [`SCHEMA_VERSION`].
pub fn validate_schema_version(version: u32) -> Result<(), SchemaError> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(SchemaError::UnsupportedVersion {
            found: version,
            expected: SCHEMA_VERSION,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn round_trip_full_document() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "ci".to_owned(),
            TaskDefinition {
                description: Some("Run CI".to_owned()),
                depends_on: vec!["fmt".to_owned(), "test".to_owned()],
                app: "test".to_owned(),
                working_directory: Some("flake-root".to_owned()),
                hidden: false,
                category: Some("validation".to_owned()),
            },
        );
        let doc = TaskDocument::new(tasks);

        let encoded = serde_json::to_value(&doc).expect("serialize");
        let decoded: TaskDocument = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, doc);
        decoded.validate().expect("schema version 1 is supported");
    }

    #[test]
    fn depends_on_defaults_to_empty() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "test": {
                    "app": "test"
                }
            }
        });
        let doc: TaskDocument = serde_json::from_value(value).expect("deserialize");
        let task = doc.tasks.get("test").expect("task present");
        assert!(task.depends_on.is_empty());
        assert!(!task.hidden);
        assert_eq!(task.app, "test");
    }

    #[test]
    fn rejects_missing_app() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "ci": {
                    "description": "missing app",
                    "dependsOn": ["test"]
                }
            }
        });
        let err = serde_json::from_value::<TaskDocument>(value).expect_err("app required");
        let message = err.to_string();
        assert!(
            message.contains("app") || message.contains("missing field"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn validate_schema_version_accepts_v1() {
        validate_schema_version(1).expect("v1 supported");
        TaskDocument::new(BTreeMap::new())
            .validate()
            .expect("new document is valid");
    }

    #[test]
    fn validate_schema_version_rejects_unsupported_major() {
        let err = validate_schema_version(2).expect_err("v2 unsupported");
        assert_eq!(
            err,
            SchemaError::UnsupportedVersion {
                found: 2,
                expected: 1,
            }
        );

        let doc = TaskDocument {
            schema_version: 99,
            tasks: BTreeMap::new(),
        };
        let err = doc.validate().expect_err("major 99 unsupported");
        assert!(matches!(
            err,
            SchemaError::UnsupportedVersion {
                found: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn serialized_field_names_use_camel_case_vocab() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "build".to_owned(),
            TaskDefinition {
                description: None,
                depends_on: vec!["fmt".to_owned()],
                app: "build".to_owned(),
                working_directory: Some("invocation".to_owned()),
                hidden: true,
                category: None,
            },
        );
        let value = serde_json::to_value(TaskDocument::new(tasks)).expect("serialize");
        let task = &value["tasks"]["build"];
        assert!(task.get("dependsOn").is_some());
        assert!(task.get("workingDirectory").is_some());
        assert!(task.get("depends_on").is_none());
        assert_eq!(task["hidden"], Value::Bool(true));
    }
}
