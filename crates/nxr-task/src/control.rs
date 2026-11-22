//! `nxr-control-v1` child messages.
//!
//! Stage messages become [`ProgressUpdate`] values already bound to the node
//! that spawned the child. Diagnostics and artifact declarations are untrusted
//! side data. Nothing in this module can set a node outcome, a run outcome, or
//! a verified artifact.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::progress::{PROGRESS_V1, ProgressUpdate};

/// Schema identifier required on every control message.
pub const CONTROL_V1: &str = "nxr-control-v1";

const STAGE_KINDS: &[&str] = &["stage_start", "stage_update", "stage_complete"];

/// Severity carried by an untrusted child diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// A child diagnostic. It does not fail a node by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: Option<String>,
    pub message: String,
}

/// A child artifact declaration. [`Self::is_verified`] is always false.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedArtifactDeclaration {
    pub artifact: Map<String, Value>,
}

impl UntrustedArtifactDeclaration {
    /// Child JSON cannot satisfy artifact verification.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        false
    }
}

/// What a parsed control message is allowed to affect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlOutcome {
    /// `stage_start`, `stage_update`, or `stage_complete` for the spawn node.
    Progress(ProgressUpdate),
    /// Untrusted diagnostic, including an ignored unknown kind.
    Diagnostic(UntrustedDiagnostic),
    /// Untrusted, unverified artifact declaration.
    ArtifactDeclaration(UntrustedArtifactDeclaration),
}

/// Parse one `nxr-control-v1` message and bind it to `spawn_node_id`.
///
/// A `node_id` that does not match the spawn is rejected. Unknown kinds are
/// ignored and returned as diagnostics. Stage messages become progress
/// snapshots. This function does not return a node outcome, a run outcome, or
/// a verified artifact.
///
/// # Errors
///
/// Returns an error when the frame is not a single valid message for `spawn_node_id`.
#[must_use = "a rejected control message must not be applied"]
pub fn parse_control_message(bytes: &[u8], spawn_node_id: &str) -> Result<ControlOutcome, String> {
    if spawn_node_id.is_empty() {
        return Err("spawn node id is empty".into());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| "control message is not utf-8".to_string())?;
    let value: Value =
        serde_json::from_str(text).map_err(|err| format!("control message is not json: {err}"))?;
    let object = value
        .as_object()
        .ok_or("control message must be an object")?;
    let schema = required_string(object, "schema_version")?;
    if schema != CONTROL_V1 {
        return Err(format!("unsupported control schema `{schema}`"));
    }
    let node_id = required_string(object, "node_id")?;
    if node_id != spawn_node_id {
        return Err(format!(
            "control message for node `{node_id}` does not match spawn `{spawn_node_id}`"
        ));
    }
    let kind = required_string(object, "kind")?;
    if STAGE_KINDS.contains(&kind) {
        parse_stage(object)
    } else if kind == "diagnostic" {
        parse_diagnostic(object)
    } else if kind == "artifact_declaration" {
        parse_artifact(object)
    } else {
        let shown: String = kind.chars().take(128).collect();
        Ok(ControlOutcome::Diagnostic(UntrustedDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: Some("unknown_control_kind".into()),
            message: format!("ignored unknown control kind `{shown}`"),
        }))
    }
}

fn parse_stage(object: &Map<String, Value>) -> Result<ControlOutcome, String> {
    reject_unknown(
        object,
        &[
            "schema_version",
            "kind",
            "node_id",
            "stage_id",
            "stage_label",
            "completed",
            "total",
            "unit",
            "state",
        ],
    )?;
    let _ = required_string(object, "stage_id")?;
    let _ = required_string(object, "stage_label")?;
    if !object.contains_key("unit") {
        return Err("control message field `unit` is required".into());
    }
    if !object.contains_key("state") {
        return Err("control message field `state` is required".into());
    }
    let mut body = Map::new();
    body.insert(
        "schema_version".into(),
        Value::String(PROGRESS_V1.to_owned()),
    );
    for field in [
        "node_id",
        "stage_id",
        "stage_label",
        "completed",
        "total",
        "unit",
        "state",
    ] {
        if let Some(value) = object.get(field) {
            body.insert(field.to_owned(), value.clone());
        }
    }
    let update: ProgressUpdate = serde_json::from_value(Value::Object(body))
        .map_err(|err| format!("invalid stage progress: {err}"))?;
    update.validate()?;
    Ok(ControlOutcome::Progress(update))
}

fn parse_diagnostic(object: &Map<String, Value>) -> Result<ControlOutcome, String> {
    reject_unknown(
        object,
        &[
            "schema_version",
            "kind",
            "node_id",
            "severity",
            "code",
            "message",
        ],
    )?;
    let severity = match object.get("severity").and_then(Value::as_str) {
        Some("info") => DiagnosticSeverity::Info,
        Some("warning") => DiagnosticSeverity::Warning,
        Some("error") => DiagnosticSeverity::Error,
        _ => return Err("control diagnostic severity is invalid".into()),
    };
    let message = match object.get("message") {
        Some(Value::String(message)) => message.clone(),
        Some(_) => return Err("control diagnostic message must be a string".into()),
        None => return Err("control message field `message` is required".into()),
    };
    let code = match object.get("code") {
        None => None,
        Some(Value::String(code)) => Some(code.clone()),
        Some(_) => return Err("control diagnostic code must be a string".into()),
    };
    Ok(ControlOutcome::Diagnostic(UntrustedDiagnostic {
        severity,
        code,
        message,
    }))
}

fn parse_artifact(object: &Map<String, Value>) -> Result<ControlOutcome, String> {
    reject_unknown(object, &["schema_version", "kind", "node_id", "artifact"])?;
    let artifact = object
        .get("artifact")
        .and_then(Value::as_object)
        .ok_or("control artifact declaration must be an object")?;
    Ok(ControlOutcome::ArtifactDeclaration(
        UntrustedArtifactDeclaration {
            artifact: artifact.clone(),
        },
    ))
}

fn reject_unknown(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("control message field `{unknown}` is not allowed"));
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    match object.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.as_str()),
        Some(Value::String(_)) => Err(format!("control message field `{field}` is empty")),
        Some(_) => Err(format!("control message field `{field}` must be a string")),
        None => Err(format!("control message field `{field}` is required")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::{ProgressState, ProgressUnit, StandardUnit};

    fn stage(kind: &str, node: &str) -> String {
        format!(
            r#"{{"schema_version":"nxr-control-v1","kind":"{kind}","node_id":"{node}","stage_id":"nextest","stage_label":"NXR compiler and lint","completed":187,"total":232,"unit":"tests","state":"running"}}"#
        )
    }

    #[test]
    fn stage_messages_become_progress_bound_to_the_spawn() {
        for kind in ["stage_start", "stage_update", "stage_complete"] {
            let outcome = parse_control_message(stage(kind, "rust-tests").as_bytes(), "rust-tests")
                .expect("stage");
            match outcome {
                ControlOutcome::Progress(update) => {
                    assert_eq!(update.schema_version, PROGRESS_V1);
                    assert_eq!(update.node_id, "rust-tests");
                    assert_eq!(update.stage_id, "nextest");
                    assert_eq!(update.completed, Some(187));
                    assert_eq!(update.total, Some(232));
                    assert_eq!(update.unit, ProgressUnit::Standard(StandardUnit::Tests));
                    assert_eq!(update.state, ProgressState::Running);
                    assert!(!update.is_indeterminate());
                }
                other => panic!("stage kind {kind} became {other:?}"),
            }
        }
    }

    #[test]
    fn a_child_node_that_does_not_match_the_spawn_is_rejected() {
        let error =
            parse_control_message(stage("stage_update", "other-node").as_bytes(), "rust-tests")
                .expect_err("mismatch");
        assert!(error.contains("does not match spawn"));
        let unknown = r#"{"schema_version":"nxr-control-v1","kind":"run_completed","node_id":"other-node","success":true}"#;
        assert!(parse_control_message(unknown.as_bytes(), "rust-tests").is_err());
        assert!(parse_control_message(&[0xff, 0xfe], "rust-tests").is_err());
        assert!(parse_control_message(stage("stage_update", "rust-tests").as_bytes(), "").is_err());
    }

    #[test]
    fn diagnostics_and_artifact_declarations_stay_untrusted() {
        let diagnostic = r#"{"schema_version":"nxr-control-v1","kind":"diagnostic","node_id":"rust-tests","severity":"error","code":"E1","message":"boom"}"#;
        match parse_control_message(diagnostic.as_bytes(), "rust-tests").expect("diagnostic") {
            ControlOutcome::Diagnostic(diagnostic) => {
                assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
                assert_eq!(diagnostic.code.as_deref(), Some("E1"));
                assert_eq!(diagnostic.message, "boom");
            }
            other => panic!("diagnostic became {other:?}"),
        }
        let artifact = r#"{"schema_version":"nxr-control-v1","kind":"artifact_declaration","node_id":"rust-tests","artifact":{"verified":true,"name":"out"}}"#;
        match parse_control_message(artifact.as_bytes(), "rust-tests").expect("artifact") {
            ControlOutcome::ArtifactDeclaration(declaration) => {
                assert!(!declaration.is_verified());
                assert_eq!(declaration.artifact["verified"], Value::Bool(true));
                assert_eq!(declaration.artifact["name"], "out");
            }
            other => panic!("artifact became {other:?}"),
        }
    }

    #[test]
    fn a_child_cannot_set_an_outcome_or_smuggle_one_into_a_stage() {
        let forged = r#"{"schema_version":"nxr-control-v1","kind":"run_completed","node_id":"rust-tests","success":true,"status":"succeeded"}"#;
        match parse_control_message(forged.as_bytes(), "rust-tests").expect("ignored") {
            ControlOutcome::Diagnostic(diagnostic) => {
                assert_eq!(diagnostic.code.as_deref(), Some("unknown_control_kind"));
                assert!(diagnostic.message.contains("run_completed"));
            }
            other => panic!("forged outcome became {other:?}"),
        }
        let smuggled = r#"{"schema_version":"nxr-control-v1","kind":"stage_complete","node_id":"rust-tests","stage_id":"nextest","stage_label":"NXR compiler and lint","unit":"tests","state":"succeeded","outcome":"succeeded"}"#;
        assert!(parse_control_message(smuggled.as_bytes(), "rust-tests").is_err());
        let missing_total = r#"{"schema_version":"nxr-control-v1","kind":"stage_update","node_id":"rust-tests","stage_id":"nextest","stage_label":"NXR compiler and lint","completed":3,"unit":"steps","state":"running"}"#;
        match parse_control_message(missing_total.as_bytes(), "rust-tests").expect("open count") {
            ControlOutcome::Progress(update) => assert!(update.is_indeterminate()),
            other => panic!("indeterminate stage became {other:?}"),
        }
    }
}
