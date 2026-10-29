//! Durable run record hooks (V4.1 scaffolding).
//!
//! Session supervisor records under `supervisor/` remain the live owner for
//! `nxr session`. This module defines the on-disk shape for longer-lived runs
//! (`nxr runs`, reconnect, chunked logs) without requiring `nxrd` for direct
//! `nix run`.
//!
//! Persistence is **journal-first**: each transition appends one JSONL line with
//! a monotonic `sequence`, then materializes `{run_id}.json`. A per-run lock
//! serializes writers in-process; cross-host coordination is out of scope here.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::state_dir;
use crate::operation::Operation;
use crate::operation_ir::{OPERATION_IR_SCHEMA_VERSION, OperationIrEnvelope};

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
    #[cfg(test)]
    if TEST_FAIL_AFTER_JOURNAL.with(|cell| cell.get()) {
        return Err(io::Error::other("injected snapshot failure"));
    }
    let dir = durable_runs_dir();
    fs::create_dir_all(&dir)?;
    let path = durable_run_path(&dir, &record.run_id);
    write_atomic_json(&path, record)
}

/// Load one durable run record.
///
/// When `{run_id}.json` is missing but the events journal exists, the record is
/// rebuilt from journal lines (crash between journal append and snapshot write).
///
/// # Errors
///
/// Returns [`io::Error`] when the record is missing or invalid JSON.
pub fn load_durable_run(run_id: &str) -> io::Result<DurableRunRecord> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    let path = durable_run_path(&dir, run_id);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let record: DurableRunRecord = serde_json::from_str(&text)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            reconcile_record_with_journal(run_id, record)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            materialize_record_from_journal(run_id)
        }
        Err(err) => Err(err),
    }
}

/// One append-only journal line beside `{run_id}.json` (JSONL).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DurableRunJournalLine {
    pub sequence: u64,
    pub kind: String,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<DurableRunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

/// Remove durable metadata and journal for one run (best-effort cleanup).
pub fn remove_durable_run_artifacts(run_id: &str) {
    if validate_run_id(run_id).is_err() {
        return;
    }
    let dir = durable_runs_dir();
    let _ = fs::remove_file(durable_run_path(&dir, run_id));
    let _ = fs::remove_file(durable_events_path(&dir, run_id));
    let _ = fs::remove_file(run_lock_path(&dir, run_id));
}

/// Create or replace a durable record when the session supervisor starts a run.
///
/// Supervisor JSON remains authoritative for the live process; durable metadata and
/// the events journal must stay consistent or callers fail the start.
pub fn sync_supervisor_started(run_id: &str, command: &[String]) -> io::Result<()> {
    validate_run_id(run_id)?;
    let operation = Operation::NativeCommand {
        argv: command.to_vec(),
    };
    with_run_lock(run_id, || {
        let mut record = DurableRunRecord::pending(run_id, &operation);
        record.status = DurableRunStatus::Running;
        let sequence = next_journal_sequence(run_id)?;
        append_journal_line(
            run_id,
            DurableRunJournalLine {
                sequence,
                kind: "supervisor_started".into(),
                at: unix_now_secs(),
                status: Some(DurableRunStatus::Running),
                exit_code: None,
                command: Some(command.to_vec()),
            },
        )?;
        record.event_sequence = sequence;
        store_durable_run(&record)
    })
}

/// Update durable lifecycle when the supervisor run finishes or is cancelled.
pub fn sync_supervisor_terminal(
    run_id: &str,
    status: DurableRunStatus,
    exit_code: Option<i32>,
) -> io::Result<()> {
    validate_run_id(run_id)?;
    with_run_lock(run_id, || {
        let mut record = load_durable_run_or_default(run_id)?;
        record.status = status;
        record.exit_code = exit_code;
        let sequence = next_journal_sequence(run_id)?;
        append_journal_line(
            run_id,
            DurableRunJournalLine {
                sequence,
                kind: "supervisor_terminal".into(),
                at: unix_now_secs(),
                status: Some(status),
                exit_code,
                command: None,
            },
        )?;
        record.event_sequence = sequence;
        store_durable_run(&record)
    })
}

/// Log terminal sync failure once, then retry a single reconcile write.
pub fn sync_supervisor_terminal_with_reconcile(
    run_id: &str,
    status: DurableRunStatus,
    exit_code: Option<i32>,
) -> io::Result<()> {
    match sync_supervisor_terminal(run_id, status, exit_code) {
        Ok(()) => Ok(()),
        Err(first) => {
            eprintln!(
                "nxr durable run terminal sync failed for `{run_id}`: {first}; reconciling once"
            );
            sync_supervisor_terminal(run_id, status, exit_code)
        }
    }
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

fn durable_events_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("{run_id}.events.jsonl"))
}

fn run_lock_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("{run_id}.lock"))
}

fn with_run_lock<T>(run_id: &str, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    fs::create_dir_all(&dir)?;
    let lock_path = run_lock_path(&dir, run_id);
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    let result = operation();
    let _ = lock_file.unlock();
    result
}

fn read_journal_lines(run_id: &str) -> io::Result<Vec<DurableRunJournalLine>> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    let path = durable_events_path(&dir, run_id);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
        })
        .collect()
}

fn next_journal_sequence(run_id: &str) -> io::Result<u64> {
    let lines = read_journal_lines(run_id)?;
    Ok(lines
        .iter()
        .map(|line| line.sequence)
        .max()
        .unwrap_or(0)
        .saturating_add(1))
}

fn load_durable_run_or_default(run_id: &str) -> io::Result<DurableRunRecord> {
    match load_durable_run(run_id) {
        Ok(record) => Ok(record),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let operation = Operation::NativeCommand { argv: Vec::new() };
            let mut pending = DurableRunRecord::pending(run_id, &operation);
            pending.status = DurableRunStatus::Running;
            Ok(pending)
        }
        Err(err) => Err(err),
    }
}

fn reconcile_record_with_journal(
    run_id: &str,
    mut record: DurableRunRecord,
) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines(run_id)?;
    let Some(last) = lines.iter().max_by_key(|line| line.sequence) else {
        return Ok(record);
    };
    if last.sequence > record.event_sequence {
        record.event_sequence = last.sequence;
        if let Some(status) = last.status {
            record.status = status;
        }
        if last.exit_code.is_some() {
            record.exit_code = last.exit_code;
        }
    }
    Ok(record)
}

fn materialize_record_from_journal(run_id: &str) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines(run_id)?;
    if lines.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "durable run record missing",
        ));
    }
    let last = lines
        .iter()
        .max_by_key(|line| line.sequence)
        .expect("non-empty journal");
    let started = lines
        .iter()
        .find(|line| line.kind == "supervisor_started")
        .and_then(|line| line.command.as_ref());
    let operation = Operation::NativeCommand {
        argv: started.cloned().unwrap_or_default(),
    };
    let mut record = DurableRunRecord::pending(run_id, &operation);
    record.status = last.status.unwrap_or(DurableRunStatus::Running);
    record.exit_code = last.exit_code;
    record.event_sequence = last.sequence;
    record.created_at = lines
        .first()
        .map(|line| line.at)
        .unwrap_or_else(unix_now_secs);
    Ok(record)
}

fn append_journal_line(run_id: &str, line: DurableRunJournalLine) -> io::Result<()> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    fs::create_dir_all(&dir)?;
    let path = durable_events_path(&dir, run_id);
    let body = serde_json::to_string(&line)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(body.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
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
thread_local! {
    static TEST_FAIL_AFTER_JOURNAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn set_test_durable_runs_dir(path: Option<PathBuf>) {
    TEST_DURABLE_RUNS_DIR.with(|cell| *cell.borrow_mut() = path);
}

#[cfg(test)]
pub(crate) fn set_test_fail_durable_snapshot_after_journal(fail: bool) {
    TEST_FAIL_AFTER_JOURNAL.with(|cell| cell.set(fail));
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
            let record = DurableRunRecord::pending(
                "run-test-1",
                &Operation::FlakeApp {
                    name: "lint".to_owned(),
                },
            );
            store_durable_run(&record).unwrap();
            let restored = load_durable_run("run-test-1").unwrap();
            assert_eq!(restored.schema_version, DURABLE_RUN_SCHEMA_VERSION);
            assert_eq!(
                restored.operation.schema_version,
                OPERATION_IR_SCHEMA_VERSION
            );
            assert_eq!(restored.status, DurableRunStatus::Pending);
        });
    }

    #[test]
    fn hub_preview_does_not_embed_fleet_assignment_fields() {
        let record = DurableRunRecord::pending(
            "run-preview",
            &Operation::NixBuild {
                installable: ".#checks".to_owned(),
            },
        );
        let preview = hub_inspector_operation_preview(&record);
        let text = preview.to_string();
        assert_eq!(preview["operation"], "nix_build");
        assert!(!text.contains("Assignment"));
    }

    #[test]
    fn store_and_load_reject_path_traversal_run_ids() {
        with_temp_runs_dir(|| {
            let record = DurableRunRecord::pending(
                "../escape",
                &Operation::FlakeApp {
                    name: "x".to_owned(),
                },
            );
            let err = store_durable_run(&record).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

            for bad in ["", "a/b", "..", "run/../other"] {
                let err = load_durable_run(bad).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "load {bad:?}");
                let err = validate_run_id(bad).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "validate {bad:?}");
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

    #[test]
    fn supervisor_sync_appends_monotonic_events_jsonl() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-journal", &["echo".into(), "hi".into()]).unwrap();
            sync_supervisor_terminal("run-journal", DurableRunStatus::Succeeded, Some(0)).unwrap();
            sync_supervisor_terminal("run-journal", DurableRunStatus::Failed, Some(2)).unwrap();

            let path = durable_events_path(&durable_runs_dir(), "run-journal");
            let text = fs::read_to_string(path).unwrap();
            let lines: Vec<DurableRunJournalLine> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 3);
            assert_eq!(lines[0].kind, "supervisor_started");
            assert_eq!(lines[0].sequence, 1);
            assert_eq!(lines[1].kind, "supervisor_terminal");
            assert_eq!(lines[1].sequence, 2);
            assert_eq!(lines[2].sequence, 3);
            assert!(lines[1].sequence < lines[2].sequence);

            let record = load_durable_run("run-journal").unwrap();
            assert_eq!(record.event_sequence, 3);
        });
    }

    #[test]
    fn load_recovers_from_journal_when_json_snapshot_missing() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-recover", &["echo".into(), "ok".into()]).unwrap();
            sync_supervisor_terminal("run-recover", DurableRunStatus::Succeeded, Some(0)).unwrap();
            let json_path = durable_run_path(&durable_runs_dir(), "run-recover");
            fs::remove_file(json_path).unwrap();
            let record = load_durable_run("run-recover").unwrap();
            assert_eq!(record.event_sequence, 2);
            assert_eq!(record.status, DurableRunStatus::Succeeded);
            assert_eq!(record.exit_code, Some(0));
            match &record.operation.operation {
                Operation::NativeCommand { argv } => {
                    assert_eq!(argv, &["echo".to_owned(), "ok".to_owned()]);
                }
                other => panic!("unexpected operation {other:?}"),
            }
        });
    }

    #[test]
    fn journal_line_precedes_snapshot_on_disk() {
        with_temp_runs_dir(|| {
            set_test_fail_durable_snapshot_after_journal(true);
            let err = sync_supervisor_started("run-crash", &["true".into()]).unwrap_err();
            set_test_fail_durable_snapshot_after_journal(false);
            assert!(err.to_string().contains("injected snapshot failure"), "{err}");

            let journal = durable_events_path(&durable_runs_dir(), "run-crash");
            assert!(journal.is_file());
            let json_path = durable_run_path(&durable_runs_dir(), "run-crash");
            assert!(!json_path.exists());

            let record = load_durable_run("run-crash").unwrap();
            assert_eq!(record.status, DurableRunStatus::Running);
            assert_eq!(record.event_sequence, 1);
        });
    }
}
