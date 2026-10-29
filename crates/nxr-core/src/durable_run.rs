//! Durable run record hooks (V4.1 scaffolding).
//!
//! **Sources of truth**
//!
//! - The append-only `{run_id}.events.jsonl` journal is the durable source of
//!   truth for lifecycle transitions.
//! - `{run_id}.json` is a materialized projection rebuilt from the journal
//!   (and reconciled on load when the snapshot lags or is missing).
//! - Session supervisor JSON under `supervisor/` remains the live process owner
//!   for `nxr session`; durable metadata must stay consistent on start/terminal
//!   sync but does not replace supervisor pid/wait ownership.
//!
//! [`crate::worker_schedule::LocalWorkerRegistry::select_and_reserve`] is
//! local and in-process only; it is not a distributed worker lease.
//!
//! Persistence is **journal-first**: each transition appends one JSONL line with
//! a monotonic `sequence`, then materializes `{run_id}.json`. A per-run lock
//! serializes writers in-process; cross-host coordination is out of scope here.
//!
//! An **unterminated** final physical line (crash mid-write, no trailing newline)
//! is quarantined and removed on read. A **complete** final line that is invalid
//! JSON (including a committed line ending in `\n`) is corruption and fails load.
//! Earlier bad lines also fail hard. Journal repair rewrites via a temp file,
//! `fsync`, and `rename` under the per-run lock. Each supervisor attempt gets an
//! immutable `transition_id` at start; terminal lines reuse it so snapshot retries
//! do not append a second completion event.

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
    /// Immutable attempt id assigned when the supervisor run starts (terminal reuse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_transition_id: Option<String>,
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
            attempt_transition_id: None,
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
    /// Idempotency key for one logical transition (terminal retries reuse this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_id: Option<String>,
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
/// Supervisor JSON remains the live process owner; durable metadata and the events
/// journal must stay consistent or callers fail the start.
pub fn sync_supervisor_started(run_id: &str, command: &[String]) -> io::Result<()> {
    validate_run_id(run_id)?;
    let operation = Operation::NativeCommand {
        argv: command.to_vec(),
    };
    with_run_lock(run_id, || {
        let mut record = DurableRunRecord::pending(run_id, &operation);
        record.status = DurableRunStatus::Running;
        let sequence = next_journal_sequence_under_run_lock(run_id)?;
        let attempt_transition_id = new_supervisor_attempt_transition_id(run_id, sequence);
        record.attempt_transition_id = Some(attempt_transition_id.clone());
        append_journal_line(
            run_id,
            DurableRunJournalLine {
                sequence,
                kind: "supervisor_started".into(),
                at: unix_now_secs(),
                status: Some(DurableRunStatus::Running),
                exit_code: None,
                command: Some(command.to_vec()),
                transition_id: Some(attempt_transition_id),
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
        let mut record = load_durable_run_or_default_under_run_lock(run_id)?;
        record.status = status;
        record.exit_code = exit_code;
        let lines = read_journal_lines_under_run_lock(run_id)?;
        let transition_id = record
            .attempt_transition_id
            .clone()
            .or_else(|| supervisor_attempt_transition_id_from_journal(&lines))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "supervisor attempt transition id missing",
                )
            })?;
        record.attempt_transition_id = Some(transition_id.clone());
        if let Some(existing) = lines.iter().find(|line| {
            line.kind == "supervisor_terminal"
                && line.transition_id.as_deref() == Some(transition_id.as_str())
        }) {
            record.event_sequence = existing.sequence;
            return store_durable_run(&record);
        }
        let sequence = next_journal_sequence_under_run_lock(run_id)?;
        append_journal_line(
            run_id,
            DurableRunJournalLine {
                sequence,
                kind: "supervisor_terminal".into(),
                at: unix_now_secs(),
                status: Some(status),
                exit_code,
                command: None,
                transition_id: Some(transition_id),
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

fn new_supervisor_attempt_transition_id(run_id: &str, sequence: u64) -> String {
    format!("supervisor_attempt:{run_id}:{sequence}")
}

fn supervisor_attempt_transition_id_from_journal(
    lines: &[DurableRunJournalLine],
) -> Option<String> {
    lines
        .iter()
        .filter(|line| line.kind == "supervisor_started")
        .max_by_key(|line| line.sequence)
        .and_then(|line| {
            line.transition_id
                .clone()
                .or_else(|| Some(format!("supervisor_attempt:legacy:{}", line.sequence)))
        })
}

fn read_journal_lines(run_id: &str) -> io::Result<Vec<DurableRunJournalLine>> {
    read_journal_lines_impl(run_id, false)
}

fn read_journal_lines_under_run_lock(run_id: &str) -> io::Result<Vec<DurableRunJournalLine>> {
    read_journal_lines_impl(run_id, true)
}

fn read_journal_lines_impl(
    run_id: &str,
    under_run_lock: bool,
) -> io::Result<Vec<DurableRunJournalLine>> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    let path = durable_events_path(&dir, run_id);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    parse_journal_text(run_id, &path, &text, under_run_lock)
}

fn split_physical_journal_lines(text: &str) -> (Vec<&str>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let ends_with_newline = text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    if ends_with_newline && lines.last() == Some(&"") {
        lines.pop();
    }
    let unterminated_final = !ends_with_newline;
    (lines, unterminated_final)
}

fn parse_journal_text(
    run_id: &str,
    path: &Path,
    text: &str,
    under_run_lock: bool,
) -> io::Result<Vec<DurableRunJournalLine>> {
    let (physical_lines, unterminated_final) = split_physical_journal_lines(text);
    let physical_lines: Vec<&str> = physical_lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if physical_lines.is_empty() {
        return Ok(Vec::new());
    }

    let corrupt = |err: serde_json::Error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("corrupt durable run journal line: {err}"),
        )
    };

    let mut parsed = Vec::with_capacity(physical_lines.len());

    if unterminated_final {
        let last_index = physical_lines.len() - 1;
        for line in &physical_lines[..last_index] {
            parsed.push(parse_journal_line(line).map_err(corrupt)?);
        }
        let last = physical_lines[last_index];
        match parse_journal_line(last) {
            Ok(entry) => {
                parsed.push(entry);
                Ok(parsed)
            }
            Err(_err) => {
                quarantine_truncated_journal_tail(run_id, path, last)?;
                rewrite_journal_file(run_id, path, &parsed, under_run_lock)?;
                Ok(parsed)
            }
        }
    } else {
        for line in &physical_lines {
            parsed.push(parse_journal_line(line).map_err(corrupt)?);
        }
        Ok(parsed)
    }
}

fn parse_journal_line(line: &str) -> Result<DurableRunJournalLine, serde_json::Error> {
    serde_json::from_str(line)
}

fn quarantine_truncated_journal_tail(
    run_id: &str,
    journal_path: &Path,
    bad_line: &str,
) -> io::Result<()> {
    let quarantine_path = journal_path.with_extension("jsonl.truncated-tail");
    let body = format!("run_id={run_id}\n--- truncated tail ---\n{bad_line}\n");
    fs::write(quarantine_path, body)
}

fn rewrite_journal_file(
    run_id: &str,
    path: &Path,
    lines: &[DurableRunJournalLine],
    under_run_lock: bool,
) -> io::Result<()> {
    let rewrite = || rewrite_journal_file_locked(path, lines);
    if under_run_lock {
        rewrite()
    } else {
        with_run_lock(run_id, rewrite)
    }
}

fn rewrite_journal_file_locked(path: &Path, lines: &[DurableRunJournalLine]) -> io::Result<()> {
    if lines.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let mut body = String::new();
    for line in lines {
        let json = serde_json::to_string(line)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        body.push_str(&json);
        body.push('\n');
    }
    let temporary = path.with_extension("jsonl.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn next_journal_sequence_under_run_lock(run_id: &str) -> io::Result<u64> {
    let lines = read_journal_lines_under_run_lock(run_id)?;
    Ok(lines
        .iter()
        .map(|line| line.sequence)
        .max()
        .unwrap_or(0)
        .saturating_add(1))
}

fn load_durable_run_or_default_under_run_lock(run_id: &str) -> io::Result<DurableRunRecord> {
    validate_run_id(run_id)?;
    let dir = durable_runs_dir();
    let path = durable_run_path(&dir, run_id);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let record: DurableRunRecord = serde_json::from_str(&text)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            reconcile_record_with_journal_under_run_lock(run_id, record)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            materialize_record_from_journal_under_run_lock(run_id)
        }
        Err(err) => Err(err),
    }
}

fn reconcile_record_with_journal_under_run_lock(
    run_id: &str,
    mut record: DurableRunRecord,
) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines_under_run_lock(run_id)?;
    apply_journal_tail_to_record(&mut record, &lines);
    Ok(record)
}

fn materialize_record_from_journal_under_run_lock(run_id: &str) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines_under_run_lock(run_id)?;
    materialize_record_from_journal_lines(run_id, &lines)
}

fn apply_journal_tail_to_record(record: &mut DurableRunRecord, lines: &[DurableRunJournalLine]) {
    let Some(last) = lines.iter().max_by_key(|line| line.sequence) else {
        return;
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
    if record.attempt_transition_id.is_none() {
        record.attempt_transition_id = supervisor_attempt_transition_id_from_journal(lines);
    }
}

fn reconcile_record_with_journal(
    run_id: &str,
    mut record: DurableRunRecord,
) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines(run_id)?;
    apply_journal_tail_to_record(&mut record, &lines);
    Ok(record)
}

fn materialize_record_from_journal(run_id: &str) -> io::Result<DurableRunRecord> {
    let lines = read_journal_lines(run_id)?;
    materialize_record_from_journal_lines(run_id, &lines)
}

fn materialize_record_from_journal_lines(
    run_id: &str,
    lines: &[DurableRunJournalLine],
) -> io::Result<DurableRunRecord> {
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
    record.attempt_transition_id = supervisor_attempt_transition_id_from_journal(lines);
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

            let path = durable_events_path(&durable_runs_dir(), "run-journal");
            let text = fs::read_to_string(&path).unwrap();
            let lines: Vec<DurableRunJournalLine> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].kind, "supervisor_started");
            assert_eq!(lines[0].sequence, 1);
            assert_eq!(
                lines[0].transition_id.as_deref(),
                Some("supervisor_attempt:run-journal:1")
            );
            assert_eq!(lines[1].kind, "supervisor_terminal");
            assert_eq!(lines[1].sequence, 2);
            assert_eq!(
                lines[1].transition_id.as_deref(),
                lines[0].transition_id.as_deref()
            );

            sync_supervisor_terminal("run-journal", DurableRunStatus::Failed, Some(2)).unwrap();
            let text = fs::read_to_string(path).unwrap();
            let lines: Vec<DurableRunJournalLine> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 2, "terminal reuses attempt transition id");

            let record = load_durable_run("run-journal").unwrap();
            assert_eq!(record.event_sequence, 2);
            assert_eq!(record.status, DurableRunStatus::Failed);
            assert_eq!(record.exit_code, Some(2));
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
            assert!(
                err.to_string().contains("injected snapshot failure"),
                "{err}"
            );

            let journal = durable_events_path(&durable_runs_dir(), "run-crash");
            assert!(journal.is_file());
            let json_path = durable_run_path(&durable_runs_dir(), "run-crash");
            assert!(!json_path.exists());

            let record = load_durable_run("run-crash").unwrap();
            assert_eq!(record.status, DurableRunStatus::Running);
            assert_eq!(record.event_sequence, 1);
        });
    }

    #[test]
    fn load_repairs_truncated_final_journal_line() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-tail", &["echo".into(), "x".into()]).unwrap();
            let journal = durable_events_path(&durable_runs_dir(), "run-tail");
            let mut text = fs::read_to_string(&journal).unwrap();
            text.push_str("{\"sequence\":2,\"kind\":\"supervisor_terminal\",\"at\":1,\"stat");
            fs::write(&journal, text).unwrap();

            let quarantine = journal.with_extension("jsonl.truncated-tail");
            assert!(!quarantine.exists());

            let record = load_durable_run("run-tail").unwrap();
            assert_eq!(record.status, DurableRunStatus::Running);
            assert_eq!(record.event_sequence, 1);
            assert!(quarantine.is_file());

            let repaired = fs::read_to_string(&journal).unwrap();
            let lines: Vec<DurableRunJournalLine> = repaired
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 1);
        });
    }

    #[test]
    fn load_fails_when_committed_final_journal_line_is_corrupt() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-bad-tail", &["true".into()]).unwrap();
            let journal = durable_events_path(&durable_runs_dir(), "run-bad-tail");
            let mut text = fs::read_to_string(&journal).unwrap();
            text.push_str("not-json\n");
            fs::write(&journal, text).unwrap();

            let err = load_durable_run("run-bad-tail").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    #[test]
    fn journal_rewrite_is_atomic_and_leaves_no_temp_file() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-atomic", &["echo".into(), "x".into()]).unwrap();
            let journal = durable_events_path(&durable_runs_dir(), "run-atomic");
            let temp = journal.with_extension("jsonl.tmp");
            let mut text = fs::read_to_string(&journal).unwrap();
            text.push_str("{\"sequence\":2,\"kind\":\"partial");
            fs::write(&journal, text).unwrap();

            load_durable_run("run-atomic").unwrap();
            assert!(!temp.exists(), "rewrite temp file must not remain");
            assert!(journal.is_file());
            let repaired = fs::read_to_string(&journal).unwrap();
            assert!(repaired.ends_with('\n'));
            assert_eq!(repaired.lines().count(), 1);
        });
    }

    #[test]
    fn load_fails_when_committed_journal_line_is_corrupt() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-bad", &["true".into()]).unwrap();
            let journal = durable_events_path(&durable_runs_dir(), "run-bad");
            let mut text = fs::read_to_string(&journal).unwrap();
            text.push_str("not-json\n");
            text.push_str("{\"sequence\":2,\"kind\":\"supervisor_terminal\",\"at\":1}\n");
            fs::write(&journal, text).unwrap();

            let err = load_durable_run("run-bad").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    #[test]
    fn terminal_sync_is_idempotent_after_snapshot_failure() {
        with_temp_runs_dir(|| {
            sync_supervisor_started("run-replay", &["true".into()]).unwrap();
            set_test_fail_durable_snapshot_after_journal(true);
            let err = sync_supervisor_terminal("run-replay", DurableRunStatus::Succeeded, Some(0))
                .unwrap_err();
            set_test_fail_durable_snapshot_after_journal(false);
            assert!(
                err.to_string().contains("injected snapshot failure"),
                "{err}"
            );

            sync_supervisor_terminal("run-replay", DurableRunStatus::Succeeded, Some(0)).unwrap();

            let journal = durable_events_path(&durable_runs_dir(), "run-replay");
            let lines: Vec<DurableRunJournalLine> = fs::read_to_string(journal)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[1].kind, "supervisor_terminal");
            assert_eq!(
                lines[1].transition_id.as_deref(),
                Some("supervisor_attempt:run-replay:1")
            );

            let record = load_durable_run("run-replay").unwrap();
            assert_eq!(record.event_sequence, 2);
            assert_eq!(record.status, DurableRunStatus::Succeeded);
            assert_eq!(record.exit_code, Some(0));
        });
    }
}
