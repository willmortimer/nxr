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
}
