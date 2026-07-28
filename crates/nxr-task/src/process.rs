//! Long-running process node definitions (schema v2 extension).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors while validating a process node name.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProcessNameError {
    /// Empty process name.
    #[error("process name must not be empty")]
    Empty,
    /// Process name contains a path separator.
    #[error("process name `{name}` must not contain path separators")]
    PathSeparator { name: String },
    /// Process name contains parent-directory traversal.
    #[error("process name `{name}` must not contain `..`")]
    ParentTraversal { name: String },
}

/// TCP port readiness probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadinessTcp {
    pub port: u16,
}

/// HTTP readiness probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadinessHttp {
    pub url: String,
}

/// Readiness probe for a process node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessReadiness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp: Option<ReadinessTcp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<ReadinessHttp>,
}

/// Restart policy for supervised processes.
///
/// Only [`Self::Never`] is implemented. Other values are rejected at validate time
/// so execution-affecting metadata is never silently ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessRestart {
    Never,
    OnFailure,
    Always,
}

/// One long-running process node declared in `nxr.<system>.processes`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDefinition {
    pub app: String,
    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ProcessReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<ProcessRestart>,
    /// Named execution context (same machinery as tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Working directory token or flake-relative path (same as tasks).
    #[serde(
        default,
        rename = "workingDirectory",
        skip_serializing_if = "Option::is_none"
    )]
    pub working_directory: Option<String>,
    /// Extra arguments forwarded to the app after `--`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    /// Optional named `devShells.<name>` wrap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

/// Base process id from a dependency token (`database@ready` → `database`).
#[must_use]
pub fn dependency_base_name(dependency: &str) -> &str {
    dependency.split('@').next().unwrap_or(dependency)
}

/// Validate a process or task node identifier.
///
/// Rejects empty names, `/`, and `..` segments so log paths and state keys stay
/// within their intended directories.
///
/// # Errors
///
/// Returns [`ProcessNameError`] when `name` is not a safe node identifier.
pub fn validate_node_id(name: &str) -> Result<(), ProcessNameError> {
    if name.is_empty() {
        return Err(ProcessNameError::Empty);
    }
    if name.contains("..") {
        return Err(ProcessNameError::ParentTraversal {
            name: name.to_owned(),
        });
    }
    if name.contains('/') {
        return Err(ProcessNameError::PathSeparator {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Sanitize a process name for use as a single path component (log file basename).
#[must_use]
pub fn sanitize_process_log_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        "_".to_owned()
    } else {
        sanitized
    }
}

/// Parse a `processes` map from the nxr document JSON object.
#[must_use]
pub fn parse_processes(value: &serde_json::Value) -> BTreeMap<String, ProcessDefinition> {
    value
        .get("processes")
        .and_then(|processes| serde_json::from_value(processes.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ProcessNameError, ProcessReadiness, dependency_base_name, parse_processes,
        sanitize_process_log_name, validate_node_id,
    };

    #[test]
    fn parse_processes_from_document() {
        let value = json!({
            "schema_version": 2,
            "tasks": {},
            "processes": {
                "api": {
                    "app": "api-dev",
                    "dependsOn": ["database@ready"],
                    "context": "agent",
                    "workingDirectory": "flake-root",
                    "arguments": ["--port", "8080"],
                    "shell": "backend",
                    "readiness": {
                        "http": { "url": "http://127.0.0.1:8080/health" }
                    },
                    "restart": "never"
                }
            }
        });
        let processes = parse_processes(&value);
        assert_eq!(processes.len(), 1);
        let api = processes.get("api").expect("api");
        assert_eq!(api.app, "api-dev");
        assert_eq!(api.depends_on, vec!["database@ready".to_owned()]);
        assert_eq!(api.context.as_deref(), Some("agent"));
        assert_eq!(api.working_directory.as_deref(), Some("flake-root"));
        assert_eq!(api.arguments, vec!["--port".to_owned(), "8080".to_owned()]);
        assert_eq!(api.shell.as_deref(), Some("backend"));
        assert!(matches!(
            api.readiness,
            Some(ProcessReadiness {
                http: Some(_),
                tcp: None
            })
        ));
    }

    #[test]
    fn dependency_base_strips_readiness_suffix() {
        assert_eq!(dependency_base_name("database@ready"), "database");
        assert_eq!(dependency_base_name("api"), "api");
    }

    #[test]
    fn missing_processes_returns_empty() {
        let value = json!({ "schema_version": 2, "tasks": {} });
        assert!(parse_processes(&value).is_empty());
    }

    #[test]
    fn validate_node_id_rejects_unsafe_names() {
        assert!(validate_node_id("api").is_ok());
        assert_eq!(validate_node_id(""), Err(ProcessNameError::Empty));
        assert_eq!(
            validate_node_id("../escape"),
            Err(ProcessNameError::ParentTraversal {
                name: "../escape".to_owned()
            })
        );
        assert_eq!(
            validate_node_id("api/worker"),
            Err(ProcessNameError::PathSeparator {
                name: "api/worker".to_owned()
            })
        );
    }

    #[test]
    fn sanitize_process_log_name_replaces_unsafe_characters() {
        assert_eq!(sanitize_process_log_name("api"), "api");
        assert_eq!(sanitize_process_log_name("../escape"), ".._escape");
        assert_eq!(sanitize_process_log_name("///"), "___");
        assert_eq!(sanitize_process_log_name(""), "_");
    }
}
