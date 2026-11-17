//! Separately authorized remote start through an [`ExecutionTransport`].

use nxr_task::{ExecutionTransport, ProjectExecutionPolicy, submit_execution};
use serde_json::Value;

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
pub fn start_remote_authorized(
    policy: &ProjectExecutionPolicy,
    operation_id: &str,
    transport: &impl ExecutionTransport,
    body: &Value,
) -> Result<Value, String> {
    authorize_remote_start(policy, operation_id)?;
    submit_execution(transport, body)
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
        let error = start_remote_authorized(
            &policy,
            "workspace:task:marker",
            &transport,
            &body,
        )
        .expect_err("unauthorized");
        assert_eq!(error, "remote execution is not authorized");
        assert!(transport.calls.lock().expect("calls").is_empty());
    }
}
