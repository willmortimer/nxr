//! Separately authorized remote start through an [`ExecutionTransport`].

use nxr_task::{
    ExecutionSpecV2, ProjectExecutionPolicy, SourceSetV1, SourceSetV2, source_set_digest,
    source_set_digest_v2,
};
#[cfg(test)]
use nxr_task::{ExecutionTransport, submit_execution};
use serde_json::{Value, json};

/// Refuse when `[[remote_operation]]` does not list `operation_id`.
///
/// # Errors
///
/// Returns `remote execution is not authorized` when the id is absent.
pub fn authorize_remote_start(
    policy: &ProjectExecutionPolicy,
    operation_id: &str,
) -> Result<(), String> {
    if !policy.remote_allowed(operation_id) {
        return Err("remote execution is not authorized".to_owned());
    }
    Ok(())
}

/// Authorized remote submission bound to the executable spec and source digest.
#[derive(Debug)]
pub struct ValidatedRemote {
    pub operation_id: String,
    pub source_set_id: String,
    pub spec: serde_json::Value,
}

/// Bind remote authorization to the catalog id, policy, source digest, and executable spec.
///
/// # Errors
///
/// Returns on the first failed check. Never calls a transport.
pub fn validate_remote_submission(
    policy: &ProjectExecutionPolicy,
    requested_id: &str,
    catalog_operation_id: &str,
    catalog_workspace_id: &str,
    caller_workspace_id: &str,
    spec: &serde_json::Value,
    source_set: &serde_json::Value,
) -> Result<ValidatedRemote, String> {
    if requested_id != catalog_operation_id {
        return Err("remote execution is not authorized".to_owned());
    }
    if catalog_workspace_id != caller_workspace_id {
        return Err("remote execution is not authorized".to_owned());
    }
    if !policy.remote_allowed(requested_id) {
        return Err("remote execution is not authorized".to_owned());
    }

    let source_set_id = match source_set.get("schema_version").and_then(Value::as_str) {
        Some(version) if version == nxr_task::SOURCE_SET_V2 => {
            let parsed: SourceSetV2 = serde_json::from_value(source_set.clone())
                .map_err(|err| format!("source set: {err}"))?;
            source_set_digest_v2(&parsed)?
        }
        None | Some("source-set-v1") => {
            let parsed: SourceSetV1 = serde_json::from_value(source_set.clone())
                .map_err(|err| format!("source set: {err}"))?;
            source_set_digest(&parsed)
        }
        Some(_) => return Err("source set schema is not supported".to_owned()),
    };

    let admitted = spec
        .get("source_set_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if admitted != source_set_id {
        return Err("source set digest does not match the admitted manifest".to_owned());
    }

    let operation_id = match spec.get("schema_version").and_then(Value::as_str) {
        Some("execution-spec-v2") => {
            let parsed: ExecutionSpecV2 = serde_json::from_value(spec.clone())
                .map_err(|err| format!("execution spec: {err}"))?;
            if parsed.operation_id != parsed.recomputed_operation_id() {
                return Err("remote execution is not authorized".to_owned());
            }
            let name = requested_id.rsplit(':').next().unwrap_or(requested_id);
            if parsed.task_id != name {
                return Err("remote execution is not authorized".to_owned());
            }
            parsed.operation_id
        }
        None => {
            let spec_operation_id = spec
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if spec_operation_id != requested_id {
                return Err("remote execution is not authorized".to_owned());
            }
            requested_id.to_owned()
        }
        Some(_) => return Err("execution spec schema is not supported".to_owned()),
    };

    let mut out_spec = spec.clone();
    if let Some(object) = out_spec.as_object_mut() {
        object.insert("source_set_id".to_owned(), json!(source_set_id.clone()));
    }

    Ok(ValidatedRemote {
        operation_id,
        source_set_id,
        spec: out_spec,
    })
}

/// Authorize, then submit. An authorization failure never calls the transport.
///
/// # Errors
///
/// Returns the authorization error or a transport error.
#[cfg(test)]
pub fn start_remote_authorized(
    policy: &ProjectExecutionPolicy,
    operation_id: &str,
    transport: &impl ExecutionTransport,
    body: &Value,
) -> Result<Value, String> {
    authorize_remote_start(policy, operation_id)?;
    submit_execution(transport, body)
}

/// Copy `idempotency_key` into the spec object that Hub will admit.
#[must_use]
pub fn spec_with_idempotency(spec: &Value, idempotency_key: &str) -> Value {
    let mut copy = spec.clone();
    if let Some(object) = copy.as_object_mut() {
        object.insert("idempotency_key".to_owned(), json!(idempotency_key));
    }
    copy
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use nxr_task::{ExecutionTransport, SOURCE_SET_V2, SourceMemberV2};
    use serde_json::{Value, json};

    use super::*;

    struct RecordingTransport {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl ExecutionTransport for RecordingTransport {
        fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((method.to_owned(), body.clone()));
            Ok(json!({ "accepted": true }))
        }
    }

    fn policy_allowing_remote(operation_id: &str) -> ProjectExecutionPolicy {
        let root = tempfile::tempdir().unwrap();
        let policy_dir = root.path().join(".nxr");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(
            policy_dir.join("execution-policy.toml"),
            format!("[[remote_operation]]\nid = \"{operation_id}\"\n"),
        )
        .unwrap();
        ProjectExecutionPolicy::load(root.path()).unwrap()
    }

    fn minimal_source_set_v2() -> SourceSetV2 {
        SourceSetV2 {
            schema_version: SOURCE_SET_V2.to_owned(),
            primary: "lattice".to_owned(),
            members: vec![SourceMemberV2 {
                name: "lattice".to_owned(),
                logical_path: "lattice".to_owned(),
                kind: "git".to_owned(),
                commit: "aaa".to_owned(),
                submodules: Vec::new(),
                lockfiles: Vec::new(),
                dirty_snapshot_id: None,
            }],
        }
    }

    fn minimal_execution_spec_v2(task_id: &str, source_set_id: &str) -> ExecutionSpecV2 {
        let mut spec: ExecutionSpecV2 = serde_json::from_value(json!({
            "schema_version": "execution-spec-v2",
            "source_set_id": source_set_id,
            "workflow_definition_digest": "wf_test",
            "task_id": task_id,
            "operation_id": "placeholder",
            "platform": "any",
            "capabilities": ["native_command"],
            "admission": { "cancel": "cooperative", "jobs": 1 },
            "environment": { "ordinary": {}, "bindings": [] },
            "working_directory": { "kind": "workspace_relative", "path": "." },
            "resources": { "cpu": 1, "memory_bytes": 1, "exclusive": [] },
            "secret_refs": [],
            "outputs": [],
            "idempotency_key": "key",
            "leaf": { "kind": "native_command", "argv": ["/usr/bin/true"] }
        }))
        .expect("spec");
        spec.operation_id = spec.recomputed_operation_id();
        spec
    }

    #[test]
    fn unauthorized_remote_start_does_not_call_transport() {
        let policy = ProjectExecutionPolicy::default();
        let transport = RecordingTransport {
            calls: Mutex::new(Vec::new()),
        };
        let body = json!({
            "spec": {"operation_id": "workspace:task:marker"},
            "source_set": {"kind": "fixture", "commit": "c1-golden"}
        });
        let error = start_remote_authorized(&policy, "workspace:task:marker", &transport, &body)
            .expect_err("unauthorized");
        assert_eq!(error, "remote execution is not authorized");
        assert!(transport.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn grant_a_does_not_submit_executable_b() {
        let policy = policy_allowing_remote("workspace:task:alpha");
        let transport = RecordingTransport {
            calls: Mutex::new(Vec::new()),
        };
        let source = minimal_source_set_v2();
        let digest = source_set_digest_v2(&source).expect("digest");
        let source_json = serde_json::to_value(&source).expect("source json");

        let forged = json!({
            "operation_id": "workspace:task:beta",
            "source_set_id": digest,
        });
        let error = validate_remote_submission(
            &policy,
            "workspace:task:alpha",
            "workspace:task:alpha",
            "workspace",
            "workspace",
            &forged,
            &source_json,
        )
        .expect_err("forged operation id");
        assert_eq!(error, "remote execution is not authorized");
        assert!(transport.calls.lock().expect("calls").is_empty());

        let valid = minimal_execution_spec_v2("alpha", &digest);
        assert_eq!(valid.operation_id, valid.recomputed_operation_id());
        let mut tampered = serde_json::to_value(&valid).expect("spec json");
        tampered
            .as_object_mut()
            .expect("object")
            .insert("operation_id".to_owned(), json!("op_forged"));
        let error = validate_remote_submission(
            &policy,
            "workspace:task:alpha",
            "workspace:task:alpha",
            "workspace",
            "workspace",
            &tampered,
            &source_json,
        )
        .expect_err("tampered operation id");
        assert_eq!(error, "remote execution is not authorized");
        assert!(transport.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn source_set_digest_mismatch_does_not_submit() {
        let policy = policy_allowing_remote("workspace:task:alpha");
        let transport = RecordingTransport {
            calls: Mutex::new(Vec::new()),
        };
        let source = minimal_source_set_v2();
        let source_json = serde_json::to_value(&source).expect("source json");
        let spec = json!({
            "operation_id": "workspace:task:alpha",
            "source_set_id": "src2_wrong",
        });
        let error = validate_remote_submission(
            &policy,
            "workspace:task:alpha",
            "workspace:task:alpha",
            "workspace",
            "workspace",
            &spec,
            &source_json,
        )
        .expect_err("digest mismatch");
        assert_eq!(
            error,
            "source set digest does not match the admitted manifest"
        );
        assert!(transport.calls.lock().expect("calls").is_empty());
    }
}
