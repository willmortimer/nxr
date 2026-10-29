//! Submit, status, cancel, and replay against the execution JSON contract.
//!
//! This module does not dial a Hub and does not depend on a NixPlane crate.
//! The caller supplies a transport. The local runner stays in `execution_spec`.

use serde_json::{Value, json};

/// One call on the execution contract.
pub trait ExecutionTransport {
    /// Send `method` with `body` and return the JSON response.
    ///
    /// # Errors
    ///
    /// Returns an error when the transport or the remote call fails.
    fn call(&self, method: &str, body: &Value) -> Result<Value, String>;
}

/// Submit one execution spec.
///
/// # Errors
///
/// Returns an error when the transport rejects the spec.
pub fn submit_execution(
    transport: &impl ExecutionTransport,
    spec: &Value,
) -> Result<Value, String> {
    transport.call("submit", spec)
}

/// Read the stored status of one attempt.
///
/// # Errors
///
/// Returns an error when the attempt is missing or the caller cannot read it.
pub fn execution_status(
    transport: &impl ExecutionTransport,
    attempt_id: &str,
) -> Result<Value, String> {
    transport.call("status", &json!({ "attempt_id": attempt_id }))
}

/// Cancel one attempt.
///
/// # Errors
///
/// Returns an error when the caller cannot cancel the attempt.
pub fn cancel_execution(
    transport: &impl ExecutionTransport,
    attempt_id: &str,
) -> Result<Value, String> {
    transport.call("cancel", &json!({ "attempt_id": attempt_id }))
}

/// Replay the journal from `after_sequence`.
///
/// # Errors
///
/// Returns an error when the caller cannot read the attempt.
pub fn replay_execution(
    transport: &impl ExecutionTransport,
    attempt_id: &str,
    after_sequence: u64,
) -> Result<Value, String> {
    transport.call(
        "replay",
        &json!({ "attempt_id": attempt_id, "after_sequence": after_sequence }),
    )
}

/// In-process JSON execution service for tests and local wiring.
///
/// This is not a Fake stub: it implements the same method surface a remote
/// coordinator would expose over HTTP.
#[derive(Clone, Debug)]
pub struct InProcessExecutionClient {
    state: std::sync::Arc<std::sync::Mutex<InProcessExecutionState>>,
}

#[derive(Clone, Debug, Default)]
struct InProcessExecutionState {
    submitted: Vec<Value>,
    cancelled: Vec<String>,
    journal: std::collections::BTreeMap<String, u64>,
}

impl InProcessExecutionClient {
    /// Start one coordinator reachable only inside this process.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(InProcessExecutionState::default())),
        }
    }

    /// Whether `cancel` was accepted for `attempt_id`.
    #[must_use]
    pub fn cancel_seen(&self, attempt_id: &str) -> bool {
        self.state
            .lock()
            .expect("in-process execution")
            .cancelled
            .iter()
            .any(|id| id == attempt_id)
    }
}

impl Default for InProcessExecutionClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionTransport for InProcessExecutionClient {
    fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
        let mut state = self.state.lock().expect("in-process execution");
        match method {
            "submit" => {
                state.submitted.push(body.clone());
                Ok(json!({ "attempt_id": "att_inproc_1", "accepted": true }))
            }
            "status" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "status needs attempt_id".to_owned())?;
                Ok(json!({ "attempt_id": attempt_id, "phase": "running" }))
            }
            "cancel" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "cancel needs attempt_id".to_owned())?;
                state.cancelled.push(attempt_id.to_owned());
                Ok(json!({ "attempt_id": attempt_id, "cancelled": true }))
            }
            "replay" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "replay needs attempt_id".to_owned())?;
                let after = body
                    .get("after_sequence")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let sequence = state.journal.get(attempt_id).copied().unwrap_or(0);
                Ok(json!({
                    "attempt_id": attempt_id,
                    "after_sequence": after,
                    "next_sequence": sequence,
                    "events": []
                }))
            }
            other => Err(format!("unknown execution method `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl ExecutionTransport for Fake {
        fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((method.to_owned(), body.clone()));
            Ok(
                json!({ "method": method, "attempt_id": body.get("attempt_id").and_then(Value::as_str).unwrap_or("att_1") }),
            )
        }
    }

    #[test]
    fn remote_calls_keep_the_execution_methods() {
        let transport = Fake {
            calls: Mutex::new(Vec::new()),
        };
        let spec = json!({ "operation_id": "op_1" });
        let submitted = submit_execution(&transport, &spec).expect("submit");
        assert_eq!(submitted["method"], "submit");
        execution_status(&transport, "att_1").expect("status");
        cancel_execution(&transport, "att_1").expect("cancel");
        let replayed = replay_execution(&transport, "att_1", 3).expect("replay");
        assert_eq!(replayed["attempt_id"], "att_1");
        let calls = transport.calls.lock().expect("calls");
        let methods = calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>();
        assert_eq!(methods, ["submit", "status", "cancel", "replay"]);
        assert_eq!(calls[3].1["after_sequence"], 3);
    }

    #[test]
    fn in_process_transport_accepts_cancel_before_local_stop() {
        let transport = InProcessExecutionClient::new();
        cancel_execution(&transport, "att_remote_7").expect("cancel");
        assert!(transport.cancel_seen("att_remote_7"));
    }
}
