//! Versioned operation IR envelope (V4.0 scaffolding).
//!
//! Planning desugars existing task/app/script surfaces into [`Operation`]. This
//! module adds a stable wire shape for later run-protocol and Hub inspector
//! hooks without forcing project migration.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::operation::Operation;

/// Schema version for serialized operation IR envelopes.
pub const OPERATION_IR_SCHEMA_VERSION: u32 = 1;

/// One tagged operation plus envelope metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationIrEnvelope {
    pub schema_version: u32,
    pub operation: Operation,
}

impl OperationIrEnvelope {
    /// Wrap one desugared operation for persistence or RPC.
    #[must_use]
    pub fn new(operation: &Operation) -> Self {
        Self {
            schema_version: OPERATION_IR_SCHEMA_VERSION,
            operation: operation.clone(),
        }
    }

    /// Serialize to JSON for durable run records and inspector previews.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization fails.
    pub fn to_json(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::ProcessRestartPolicy;

    #[test]
    fn envelope_carries_schema_version_and_operation() {
        let operation = Operation::Adapter {
            adapter_kind: "hub_inspector".to_owned(),
            configuration: serde_json::json!({ "view": "run" }),
        };
        let envelope = OperationIrEnvelope::new(&operation);
        let value = envelope.to_json().expect("json");
        assert_eq!(value["schema_version"], OPERATION_IR_SCHEMA_VERSION);
        assert_eq!(value["operation"]["kind"], "adapter");
        let restored: OperationIrEnvelope = serde_json::from_value(value).expect("restore");
        assert_eq!(restored.operation, operation);
    }

    #[test]
    fn fleet_assignment_types_are_not_part_of_operation_ir() {
        let sample = OperationIrEnvelope::new(&Operation::FlakeApp {
            name: "ci".to_owned(),
        })
        .to_json()
        .expect("json");
        let text = sample.to_string();
        assert!(!text.contains("Assignment"));
        assert!(!text.contains("PhysicalHost"));
        assert!(!text.contains("Profile"));
    }

    #[test]
    fn process_desugar_matches_task_app_pattern() {
        let from_app = Operation::from_process_app("web");
        assert!(matches!(
            from_app,
            Operation::Process {
                app,
                readiness,
                restart: None
            } if app == "web" && readiness.is_empty()
        ));
        let envelope = OperationIrEnvelope::new(&Operation::Process {
            app: "web".to_owned(),
            readiness: String::new(),
            restart: Some(ProcessRestartPolicy::Always),
        });
        assert_eq!(envelope.operation.kind(), "process");
    }
}
