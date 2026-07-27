//! Long-running process node definitions (schema v2 extension).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessRestart {
    Never,
    OnFailure,
    Always,
}

/// One long-running process node declared in `nxr.<system>.processes`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessDefinition {
    pub app: String,
    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ProcessReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<ProcessRestart>,
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

    use super::{ProcessReadiness, parse_processes};

    #[test]
    fn parse_processes_from_document() {
        let value = json!({
            "schema_version": 2,
            "tasks": {},
            "processes": {
                "api": {
                    "app": "api-dev",
                    "dependsOn": ["database@ready"],
                    "readiness": {
                        "http": { "url": "http://127.0.0.1:8080/health" }
                    },
                    "restart": "on-failure"
                }
            }
        });
        let processes = parse_processes(&value);
        assert_eq!(processes.len(), 1);
        let api = processes.get("api").expect("api");
        assert_eq!(api.app, "api-dev");
        assert_eq!(api.depends_on, vec!["database@ready".to_owned()]);
        assert!(matches!(
            api.readiness,
            Some(ProcessReadiness {
                http: Some(_),
                tcp: None
            })
        ));
    }

    #[test]
    fn missing_processes_returns_empty() {
        let value = json!({ "schema_version": 2, "tasks": {} });
        assert!(parse_processes(&value).is_empty());
    }
}
