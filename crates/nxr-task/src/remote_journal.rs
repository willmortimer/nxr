//! Durable binding from an NXR run to one NixPlane attempt.
//!
//! The row is written before the caller is told that submission succeeded.
//! A later call with the same idempotency key inspects that attempt.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Value, json};

use crate::remote_execution::{ExecutionTransport, submit_execution};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAttempt {
    pub idempotency_key: String,
    pub run_id: String,
    pub operation_id: String,
    pub source_set_id: String,
    pub attempt_id: Option<String>,
    pub lease_generation: Option<u64>,
    pub last_sequence: Option<u64>,
    pub phase: String,
}

/// Stable NXR run id for one idempotency key.
#[must_use]
pub fn remote_run_id(idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(idempotency_key.as_bytes());
    let mut id = String::from("run-");
    for byte in digest.iter().take(8) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Insert a submitting row, or return the row already stored for `key`.
///
/// # Errors
///
/// Returns an error when `key` was used for a different operation or source set,
/// or when the database cannot be written.
pub fn begin_remote(
    dir: &Path,
    idempotency_key: &str,
    run_id: &str,
    operation_id: &str,
    source_set_id: &str,
) -> Result<RemoteAttempt, String> {
    let mut conn = open(dir)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("begin remote attempt: {err}"))?;
    if let Some(existing) = read_key(&tx, idempotency_key)? {
        if existing.operation_id != operation_id || existing.source_set_id != source_set_id {
            return Err(format!(
                "idempotency key conflicts with {}",
                existing.run_id
            ));
        }
        return Ok(existing);
    }
    tx.execute(
        "INSERT INTO remote_attempts (
            idempotency_key, run_id, operation_id, source_set_id, attempt_id, lease_generation, phase
         ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'submitting')",
        [idempotency_key, run_id, operation_id, source_set_id],
    )
    .map_err(|err| format!("insert remote attempt: {err}"))?;
    let stored =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt missing".to_owned())?;
    tx.commit()
        .map_err(|err| format!("commit remote attempt: {err}"))?;
    Ok(stored)
}

/// Store the Hub attempt id. This happens before submission is reported.
///
/// # Errors
///
/// Returns an error when the row is missing, cancelled, or already bound to
/// a different attempt.
pub fn record_admitted(
    dir: &Path,
    idempotency_key: &str,
    attempt_id: &str,
) -> Result<RemoteAttempt, String> {
    if attempt_id.is_empty() {
        return Err("attempt id is empty".to_owned());
    }
    let mut conn = open(dir)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("admit remote attempt: {err}"))?;
    let existing =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt is missing".to_owned())?;
    if existing.phase == "cancelled" {
        return Err("remote attempt is cancelled".to_owned());
    }
    if let Some(stored) = existing.attempt_id.as_deref()
        && stored != attempt_id
    {
        return Err(format!("attempt already bound to {stored}"));
    }
    tx.execute(
        "UPDATE remote_attempts SET attempt_id = ?1, phase = 'admitted' WHERE idempotency_key = ?2",
        [attempt_id, idempotency_key],
    )
    .map_err(|err| format!("update remote attempt: {err}"))?;
    let stored =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt missing".to_owned())?;
    tx.commit()
        .map_err(|err| format!("commit remote attempt: {err}"))?;
    Ok(stored)
}

/// Mark the attempt cancelled only when NixPlane acknowledged it.
///
/// # Errors
///
/// Returns an error when the row is missing.
pub fn record_cancel(
    dir: &Path,
    idempotency_key: &str,
    acknowledged: bool,
) -> Result<RemoteAttempt, String> {
    let mut conn = open(dir)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("cancel remote attempt: {err}"))?;
    let existing =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt is missing".to_owned())?;
    if acknowledged {
        tx.execute(
            "UPDATE remote_attempts SET phase = 'cancelled' WHERE idempotency_key = ?1",
            [idempotency_key],
        )
        .map_err(|err| format!("update remote attempt: {err}"))?;
    }
    let stored = if acknowledged {
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt missing".to_owned())?
    } else {
        existing
    };
    tx.commit()
        .map_err(|err| format!("commit remote attempt: {err}"))?;
    Ok(stored)
}

/// Record a worker lease generation. An older generation cannot replace it.
///
/// # Errors
///
/// Returns `stale lease` when `generation` is older than the stored generation.
pub fn record_lease(
    dir: &Path,
    idempotency_key: &str,
    generation: u64,
) -> Result<RemoteAttempt, String> {
    let mut conn = open(dir)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("record lease: {err}"))?;
    let existing =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt is missing".to_owned())?;
    if let Some(stored) = existing.lease_generation
        && generation < stored
    {
        return Err("stale lease".to_owned());
    }
    tx.execute(
        "UPDATE remote_attempts SET lease_generation = ?1 WHERE idempotency_key = ?2",
        (generation as i64, idempotency_key),
    )
    .map_err(|err| format!("update lease: {err}"))?;
    let stored =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt missing".to_owned())?;
    tx.commit().map_err(|err| format!("commit lease: {err}"))?;
    Ok(stored)
}

/// Advance the replay cursor only when `sequence` is greater than the stored value.
///
/// A lower (or equal) sequence returns the stored row and does not move the
/// cursor backward. Callers pass the stored `last_sequence` as `--after-sequence`
/// (see `NixplaneCliTransport::replay_from`).
///
/// # Errors
///
/// Returns an error when the row is missing or the database cannot be written.
pub fn record_replay_cursor(
    dir: &Path,
    idempotency_key: &str,
    sequence: u64,
) -> Result<RemoteAttempt, String> {
    let mut conn = open(dir)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("record replay cursor: {err}"))?;
    let existing =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt is missing".to_owned())?;
    if let Some(stored) = existing.last_sequence
        && sequence <= stored
    {
        tx.commit()
            .map_err(|err| format!("commit replay cursor: {err}"))?;
        return Ok(existing);
    }
    tx.execute(
        "UPDATE remote_attempts SET last_sequence = ?1 WHERE idempotency_key = ?2",
        (sequence as i64, idempotency_key),
    )
    .map_err(|err| format!("update replay cursor: {err}"))?;
    let stored =
        read_key(&tx, idempotency_key)?.ok_or_else(|| "remote attempt missing".to_owned())?;
    tx.commit()
        .map_err(|err| format!("commit replay cursor: {err}"))?;
    Ok(stored)
}

/// Advance the stored replay cursor from a replay journal's `events` array.
///
/// Each event is an object with `sequence` and `event_type`. The cursor moves to
/// the maximum sequence. Seeing the same terminal `event_type` at the same
/// sequence again is not treated as a new event (the cursor does not move).
///
/// # Errors
///
/// Returns an error when the row is missing or the database cannot be written.
pub fn observe_replay(
    dir: &Path,
    idempotency_key: &str,
    journal: &Value,
) -> Result<RemoteAttempt, String> {
    let existing = remote_by_key(dir, idempotency_key)?
        .ok_or_else(|| "remote attempt is missing".to_owned())?;
    let Some(events) = journal.get("events").and_then(Value::as_array) else {
        return Ok(existing);
    };

    let mut max_sequence: Option<u64> = None;
    for event in events {
        let Some(sequence) = event.get("sequence").and_then(Value::as_u64) else {
            continue;
        };
        let event_type = event.get("event_type").and_then(Value::as_str);
        if is_terminal_event_type(event_type)
            && existing.last_sequence == Some(sequence)
        {
            // Same terminal event at the stored sequence is not a new event.
            continue;
        }
        max_sequence = Some(max_sequence.map_or(sequence, |current| current.max(sequence)));
    }

    match max_sequence {
        Some(sequence) => record_replay_cursor(dir, idempotency_key, sequence),
        None => Ok(existing),
    }
}

fn is_terminal_event_type(event_type: Option<&str>) -> bool {
    matches!(
        event_type,
        Some("exited" | "succeeded" | "failed" | "cancelled")
    )
}

/// Read the attempt stored for `run_id`.
///
/// # Errors
///
/// Returns an error when the database cannot be read.
pub fn remote_attempt_for_run(dir: &Path, run_id: &str) -> Result<Option<RemoteAttempt>, String> {
    let conn = open(dir)?;
    conn.query_row(
        "SELECT idempotency_key, run_id, operation_id, source_set_id, attempt_id, lease_generation, phase, last_sequence
         FROM remote_attempts WHERE run_id = ?1",
        [run_id],
        map_attempt,
    )
    .optional()
    .map_err(|err| format!("read remote attempt: {err}"))
}

/// Submit unless this key already has an attempt id.
///
/// The attempt id is stored before the returned JSON is handed to the caller.
///
/// # Errors
///
/// Returns an error when the key conflicts or the transport fails before an
/// attempt id exists.
pub fn submit_and_bind(
    dir: &Path,
    idempotency_key: &str,
    run_id: &str,
    operation_id: &str,
    source_set_id: &str,
    transport: &impl ExecutionTransport,
    body: &Value,
) -> Result<Value, String> {
    let existing = begin_remote(dir, idempotency_key, run_id, operation_id, source_set_id)?;
    if let Some(attempt_id) = existing.attempt_id.clone() {
        return Ok(json!({
            "attempt_id": attempt_id,
            "run_id": existing.run_id,
            "replayed": true,
            "phase": existing.phase,
            "lease_generation": existing.lease_generation,
        }));
    }
    match submit_execution(transport, body) {
        Ok(response) => bind_response(dir, idempotency_key, &existing.run_id, response),
        Err(err) => {
            if let Some(attempt) = remote_by_key(dir, idempotency_key)?
                && let Some(attempt_id) = attempt.attempt_id
            {
                return Ok(json!({
                    "attempt_id": attempt_id,
                    "run_id": attempt.run_id,
                    "replayed": true,
                    "phase": attempt.phase,
                }));
            }
            Err(err)
        }
    }
}

/// Cancel the stored attempt. The phase changes only after acknowledgement.
///
/// # Errors
///
/// Returns an error when no attempt id is stored or the transport fails.
pub fn cancel_and_bind(
    dir: &Path,
    idempotency_key: &str,
    transport: &impl ExecutionTransport,
) -> Result<Value, String> {
    let existing = remote_by_key(dir, idempotency_key)?
        .ok_or_else(|| "remote attempt is missing".to_owned())?;
    let attempt_id = existing
        .attempt_id
        .clone()
        .ok_or_else(|| "remote attempt has not been admitted".to_owned())?;
    let response = crate::remote_execution::cancel_execution(transport, &attempt_id)?;
    let acknowledged = response.get("cancelled").and_then(Value::as_bool) == Some(true);
    let stored = record_cancel(dir, idempotency_key, acknowledged)?;
    Ok(json!({
        "attempt_id": attempt_id,
        "run_id": stored.run_id,
        "cancelled": acknowledged,
        "cancel_complete": acknowledged,
        "phase": stored.phase,
    }))
}

fn bind_response(
    dir: &Path,
    idempotency_key: &str,
    run_id: &str,
    mut response: Value,
) -> Result<Value, String> {
    let attempt_id = response
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "submit did not return an attempt id".to_owned())?
        .to_owned();
    let stored = record_admitted(dir, idempotency_key, &attempt_id)?;
    if let Some(generation) = response.get("lease_generation").and_then(Value::as_u64) {
        record_lease(dir, idempotency_key, generation)?;
    }
    if let Some(object) = response.as_object_mut() {
        object.insert("run_id".to_owned(), json!(run_id));
        object.insert("phase".to_owned(), json!(stored.phase));
        object.insert("replayed".to_owned(), json!(false));
    }
    Ok(response)
}

fn remote_by_key(dir: &Path, idempotency_key: &str) -> Result<Option<RemoteAttempt>, String> {
    let conn = open(dir)?;
    read_key(&conn, idempotency_key)
}

fn open(dir: &Path) -> Result<Connection, String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("create run dir: {err}"))?;
    let conn = Connection::open(dir.join("remote-attempts.sqlite"))
        .map_err(|err| format!("open remote attempts: {err}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS remote_attempts (
            idempotency_key TEXT PRIMARY KEY,
            run_id TEXT NOT NULL UNIQUE,
            operation_id TEXT NOT NULL,
            source_set_id TEXT NOT NULL,
            attempt_id TEXT,
            lease_generation INTEGER,
            last_sequence INTEGER,
            phase TEXT NOT NULL
         );",
    )
    .map_err(|err| format!("prepare remote attempts: {err}"))?;
    // Existing databases created before last_sequence need an ALTER; ignore
    // duplicate-column errors when CREATE TABLE already defined the column.
    match conn.execute(
        "ALTER TABLE remote_attempts ADD COLUMN last_sequence INTEGER",
        [],
    ) {
        Ok(_) => {}
        Err(err) if err.to_string().contains("duplicate column") => {}
        Err(err) => return Err(format!("prepare remote attempts: {err}")),
    }
    Ok(conn)
}

fn read_key(conn: &Connection, idempotency_key: &str) -> Result<Option<RemoteAttempt>, String> {
    conn.query_row(
        "SELECT idempotency_key, run_id, operation_id, source_set_id, attempt_id, lease_generation, phase, last_sequence
         FROM remote_attempts WHERE idempotency_key = ?1",
        [idempotency_key],
        map_attempt,
    )
    .optional()
    .map_err(|err| format!("read remote attempt: {err}"))
}

fn map_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteAttempt> {
    let lease: Option<i64> = row.get(5)?;
    let last_sequence: Option<i64> = row.get(7)?;
    Ok(RemoteAttempt {
        idempotency_key: row.get(0)?,
        run_id: row.get(1)?,
        operation_id: row.get(2)?,
        source_set_id: row.get(3)?,
        attempt_id: row.get(4)?,
        lease_generation: lease.map(|value| value as u64),
        last_sequence: last_sequence.map(|value| value as u64),
        phase: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Scripted {
        calls: AtomicUsize,
        fail_first: bool,
    }

    impl ExecutionTransport for Scripted {
        fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if method == "cancel" {
                let acknowledged = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == "att_1");
                return Ok(json!({"attempt_id": "att_1", "cancelled": acknowledged}));
            }
            if self.fail_first && call == 0 {
                return Err("transport lost the response".to_owned());
            }
            Ok(json!({"attempt_id": "att_1", "accepted": true}))
        }
    }

    #[test]
    fn the_same_key_returns_the_stored_attempt_without_a_second_submit() {
        let dir = tempfile::tempdir().expect("temp");
        let transport = Scripted {
            calls: AtomicUsize::new(0),
            fail_first: false,
        };
        let body = json!({"spec": {"idempotency_key": "once"}});
        let first = submit_and_bind(
            dir.path(),
            "once",
            "run-once",
            "op_1",
            "src2_a",
            &transport,
            &body,
        )
        .expect("first");
        assert_eq!(first["attempt_id"], "att_1");
        assert_eq!(first["replayed"], false);
        let second = submit_and_bind(
            dir.path(),
            "once",
            "run-other",
            "op_1",
            "src2_a",
            &transport,
            &body,
        )
        .expect("second");
        assert_eq!(second["attempt_id"], "att_1");
        assert_eq!(second["replayed"], true);
        assert_eq!(second["run_id"], "run-once");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_lost_response_can_be_recovered_with_the_same_key() {
        let dir = tempfile::tempdir().expect("temp");
        let transport = Scripted {
            calls: AtomicUsize::new(0),
            fail_first: true,
        };
        let body = json!({});
        let lost = submit_and_bind(
            dir.path(),
            "once",
            "run-once",
            "op_1",
            "src2_a",
            &transport,
            &body,
        );
        assert!(lost.expect_err("lost").contains("lost"));
        let recovered = submit_and_bind(
            dir.path(),
            "once",
            "run-once",
            "op_1",
            "src2_a",
            &transport,
            &body,
        )
        .expect("recovered");
        assert_eq!(recovered["attempt_id"], "att_1");
        assert_eq!(recovered["replayed"], false);
    }

    #[test]
    fn cancel_is_complete_only_after_acknowledgement() {
        let dir = tempfile::tempdir().expect("temp");
        let transport = Scripted {
            calls: AtomicUsize::new(0),
            fail_first: false,
        };
        submit_and_bind(
            dir.path(),
            "once",
            "run-once",
            "op_1",
            "src2_a",
            &transport,
            &json!({}),
        )
        .expect("submit");
        let cancelled = cancel_and_bind(dir.path(), "once", &transport).expect("cancel");
        assert_eq!(cancelled["cancel_complete"], true);
        assert_eq!(cancelled["phase"], "cancelled");
        let row = remote_attempt_for_run(dir.path(), "run-once")
            .expect("read")
            .expect("row");
        assert_eq!(row.phase, "cancelled");
    }

    #[test]
    fn an_unacknowledged_cancel_leaves_the_attempt_admitted() {
        let dir = tempfile::tempdir().expect("temp");
        begin_remote(dir.path(), "once", "run-once", "op_1", "src2_a").expect("begin");
        record_admitted(dir.path(), "once", "att_1").expect("admit");
        let stored = record_cancel(dir.path(), "once", false).expect("cancel");
        assert_eq!(stored.phase, "admitted");
    }

    #[test]
    fn a_stale_lease_cannot_replace_the_current_generation() {
        let dir = tempfile::tempdir().expect("temp");
        begin_remote(dir.path(), "once", "run-once", "op_1", "src2_a").expect("begin");
        record_admitted(dir.path(), "once", "att_1").expect("admit");
        record_lease(dir.path(), "once", 2).expect("lease");
        let error = record_lease(dir.path(), "once", 1).expect_err("stale");
        assert_eq!(error, "stale lease");
        let row = remote_attempt_for_run(dir.path(), "run-once")
            .expect("read")
            .expect("row");
        assert_eq!(row.lease_generation, Some(2));
    }

    #[test]
    fn a_lower_replay_cursor_does_not_replace_the_stored_sequence() {
        let dir = tempfile::tempdir().expect("temp");
        begin_remote(dir.path(), "once", "run-once", "op_1", "src2_a").expect("begin");
        record_admitted(dir.path(), "once", "att_1").expect("admit");
        let advanced = record_replay_cursor(dir.path(), "once", 5).expect("cursor");
        assert_eq!(advanced.last_sequence, Some(5));
        let stored = record_replay_cursor(dir.path(), "once", 3).expect("lower");
        assert_eq!(stored.last_sequence, Some(5));
        let observed = observe_replay(
            dir.path(),
            "once",
            &json!({
                "events": [
                    {"sequence": 5, "event_type": "exited"},
                    {"sequence": 3, "event_type": "started"}
                ]
            }),
        )
        .expect("observe");
        assert_eq!(observed.last_sequence, Some(5));
    }
}
