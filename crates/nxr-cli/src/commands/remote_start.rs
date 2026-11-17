//! Separately authorized remote start through an [`ExecutionTransport`].

#[cfg(test)]
use nxr_task::{ExecutionTransport, submit_execution};
use nxr_task::{
    ProjectExecutionPolicy, SourceSetV1, SourceSetV2, source_set_digest, source_set_digest_v2,
};
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

/// Operation id and source digest from an authorized request.
///
/// # Errors
///
/// Returns an error when the spec or source set cannot be identified.
pub fn remote_identity(spec: &Value, source_set: &Value) -> Result<(String, String), String> {
    let operation_id = spec
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "execution spec is required".to_owned())?;
    let source_set_id = if source_set.get("schema_version").and_then(Value::as_str)
        == Some(nxr_task::SOURCE_SET_V2)
    {
        let parsed: SourceSetV2 = serde_json::from_value(source_set.clone())
            .map_err(|err| format!("source set: {err}"))?;
        source_set_digest_v2(&parsed)?
    } else {
        let parsed: SourceSetV1 = serde_json::from_value(source_set.clone())
            .map_err(|err| format!("source set: {err}"))?;
        source_set_digest(&parsed)
    };
    Ok((operation_id.to_owned(), source_set_id))
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

    use nxr_task::ExecutionTransport;
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
}
