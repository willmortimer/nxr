//! Durable run record hooks (V4.1 scaffolding).
//!
//! Session supervisor records under `supervisor/` remain the live owner for
//! `nxr session`. This module defines the on-disk shape for longer-lived runs
//! (`nxr runs`, reconnect, chunked logs) without requiring `nxrd` for direct
//! `nix run`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::config_dir;
use crate::operation_ir::{OperationIrEnvelope, OPERATION_IR_SCHEMA_VERSION};

/// Schema version for durable run metadata files.
pub const DURABLE_RUN_SCHEMA_VERSION: u32 = 1;

const DURABLE_RUNS_DIR: &str = "runs";

/// Lifecycle status for a durable run record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableRunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Metadata persisted for one durable run (stdout/stderr live beside this file).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DurableRunRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub status: DurableRunStatus,
    pub created_at: u64,
    pub operation: OperationIrEnvelope,
    #[serde(default)]
    pub event_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl DurableRunRecord {
    /// Create a pending record for a desugared operation.
    #[must_use]
    pub fn pending(run_id: impl Into<String>, operation: &crate::operation::Operation) -> Self {
        Self {
            schema_version: DURABLE_RUN_SCHEMA_VERSION,
            run_id: run_id.into(),
            status: DurableRunStatus::Pending,
            created_at: unix_now_secs(),
            operation: OperationIrEnvelope::new(operation),
            event_sequence: 0,
            exit_code: None,
        }
    }

    /// Append one monotonic event sequence for reconnect (V4 event envelope precursor).
    pub fn bump_event_sequence(&mut self) {
        self.event_sequence = self.event_sequence.saturating_add(1);
    }
}

/// Root directory for durable run metadata, when config dir is available.
#[must_use]
pub fn durable_runs_dir() -> PathBuf {
    config_dir().join(DURABLE_RUNS_DIR)
}

/// Persist one durable run record atomically.
///
/// # Errors
///
/// Returns [`io::Error`] when the directory or file cannot be written.
pub fn store_durable_run(record: &DurableRunRecord) -> io::Result<()> {
    let dir = durable_runs_dir();
    fs::create_dir_all(&dir)?;
    let path = durable_run_path(&dir, &record.run_id);
    write_atomic_json(&path, record)
}

/// Load one durable run record.
///
/// # Errors
///
/// Returns [`io::Error`] when the record is missing or invalid JSON.
pub fn load_durable_run(run_id: &str) -> io::Result<DurableRunRecord> {
    let dir = durable_runs_dir();
    let path = durable_run_path(&dir, run_id);
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Hub inspector hook: lossless JSON preview of one run's operation IR.
///
/// Does not dial NixPlane; callers supply Hub base URL separately
/// ([`docs/vision/V4_HUB_INSPECTOR.md`](../../../docs/vision/V4_HUB_INSPECTOR.md)).
#[must_use]
pub fn hub_inspector_operation_preview(record: &DurableRunRecord) -> Value {
    serde_json::json!({
        "schema_version": OPERATION_IR_SCHEMA_VERSION,
        "run_id": record.run_id,
        "status": record.status,
        "operation": record.operation.operation.kind(),
    })
}

fn durable_run_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("{run_id}.json"))
}

fn write_atomic_json(path: &Path, record: &DurableRunRecord) -> io::Result<()> {
    let body = serde_json::to_string_pretty(record)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body)?;
    fs::rename(temporary, path)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::Operation;

    #[test]
    fn durable_run_round_trip_uses_operation_ir_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let record = DurableRunRecord::pending("run-test-1", &Operation::FlakeApp {
            name: "lint".to_owned(),
        });
        let path = dir.path().join("run-test-1.json");
        write_atomic_json(&path, &record).unwrap();
        let text = fs::read_to_string(path).unwrap();
        let restored: DurableRunRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(restored.schema_version, DURABLE_RUN_SCHEMA_VERSION);
        assert_eq!(restored.operation.schema_version, OPERATION_IR_SCHEMA_VERSION);
        assert_eq!(restored.status, DurableRunStatus::Pending);
    }

    #[test]
    fn hub_preview_does_not_embed_fleet_assignment_fields() {
        let record = DurableRunRecord::pending("run-preview", &Operation::NixBuild {
            installable: ".#checks".to_owned(),
        });
        let preview = hub_inspector_operation_preview(&record);
        let text = preview.to_string();
        assert_eq!(preview["operation"], "nix_build");
        assert!(!text.contains("Assignment"));
    }
}
