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

use crate::config::state_dir;
use crate::operation::Operation;
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
    pub fn pending(run_id: impl Into<String>, operation: &Operation) -> Self {
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

/// Reject run ids that are not safe single path segments (matches session supervisor).
///
/// # Errors
///
/// Returns [`io::Error`] with [`io::ErrorKind::InvalidInput`] when `run_id` is rejected.
pub fn validate_run_id(run_id: &str) -> io::Result<()> {
    if run_id.is_empty() || run_id.contains('/') || run_id.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run id is not a session name",
        ));
    }
    Ok(())
}

/// Root directory for durable run metadata under the nxr state dir.
#[must_use]
pub fn durable_runs_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_DURABLE_RUNS_DIR.with(|cell| cell.borrow().clone()) {
        return path;
    }
    state_dir().join(DURABLE_RUNS_DIR)
}

/// Persist one durable run record atomically.
///
/// # Errors
///
/// Returns [`io::Error`] when the directory or file cannot be written.
pub fn store_durable_run(record: &DurableRunRecord) -> io::Result<()> {
    validate_run_id(&record.run_id)?;
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
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    let path = durable_run_path(&dir, run_id);
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Create or replace a durable record when the session supervisor starts a run.
///
/// Best-effort for reconnect metadata; supervisor JSON remains authoritative for the process.
pub fn sync_supervisor_started(run_id: &str, command: &[String]) -> io::Result<()> {
    validate_run_id(run_id)?;
    let operation = Operation::NativeCommand {
        argv: command.to_vec(),
    };
    let mut record = DurableRunRecord::pending(run_id, &operation);
    record.status = DurableRunStatus::Running;
    store_durable_run(&record)
}

/// Update durable lifecycle when the supervisor run finishes or is cancelled.
pub fn sync_supervisor_terminal(
    run_id: &str,
    status: DurableRunStatus,
    exit_code: Option<i32>,
) -> io::Result<()> {
    validate_run_id(run_id)?;
    let mut record = match load_durable_run(run_id) {
        Ok(existing) => existing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let operation = Operation::NativeCommand { argv: Vec::new() };
            let mut pending = DurableRunRecord::pending(run_id, &operation);
            pending.status = DurableRunStatus::Running;
            pending
        }
        Err(err) => return Err(err),
    };
    record.status = status;
    record.exit_code = exit_code;
    store_durable_run(&record)
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
thread_local! {
    static TEST_DURABLE_RUNS_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_durable_runs_dir(path: Option<PathBuf>) {
    TEST_DURABLE_RUNS_DIR.with(|cell| *cell.borrow_mut() = path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_runs_dir<T>(f: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        set_test_durable_runs_dir(Some(dir.path().to_path_buf()));
        let out = f();
        set_test_durable_runs_dir(None);
        out
    }

    #[test]
    fn durable_run_round_trip_uses_operation_ir_envelope() {
        with_temp_runs_dir(|| {
            let record = DurableRunRecord::pending("run-test-1", &Operation::FlakeApp {
                name: "lint".to_owned(),
            });
            store_durable_run(&record).unwrap();
            let restored = load_durable_run("run-test-1").unwrap();
            assert_eq!(restored.schema_version, DURABLE_RUN_SCHEMA_VERSION);
            assert_eq!(restored.operation.schema_version, OPERATION_IR_SCHEMA_VERSION);
            assert_eq!(restored.status, DurableRunStatus::Pending);
        });
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

    #[test]
    fn store_and_load_reject_path_traversal_run_ids() {
        with_temp_runs_dir(|| {
            let record = DurableRunRecord::pending("../escape", &Operation::FlakeApp {
                name: "x".to_owned(),
            });
            let err = store_durable_run(&record).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

            for bad in ["", "a/b", "..", "run/../other"] {
                let err = load_durable_run(bad).unwrap_err();
                assert_eq!(
                    err.kind(),
                    io::ErrorKind::InvalidInput,
                    "load {bad:?}"
                );
                let err = validate_run_id(bad).unwrap_err();
                assert_eq!(
                    err.kind(),
                    io::ErrorKind::InvalidInput,
                    "validate {bad:?}"
                );
            }
        });
    }

    #[test]
    fn durable_runs_dir_honors_test_override() {
        with_temp_runs_dir(|| {
            let path = durable_runs_dir();
            assert!(path.is_dir() || path.parent().is_some());
        });
    }
}
