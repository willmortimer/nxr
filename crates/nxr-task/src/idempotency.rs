//! Durable idempotency reservations for local MCP starts.
//!
//! The reservation lives in the run directory, not in the MCP process. A crash
//! after the insert leaves the key reserved, so a second process cannot start
//! the same work.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::catalog::{FingerprintDecision, OperationRecord, fingerprint_decision};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Reservation {
    pub run_id: String,
    pub operation_id: String,
    pub fingerprint: String,
    pub workspace_id: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Admission {
    /// This caller reserved a new run and may start it.
    Reserved(Reservation),
    /// An earlier reservation has the same fingerprint. Do not start again.
    Existing(Reservation),
    /// The key was used for different inputs.
    Conflict(Reservation),
    /// The operation is not on the execution allowlist.
    Refused,
}

/// Reserve `key` or return the earlier decision.
///
/// # Errors
///
/// Returns an error when the database cannot be opened or locked.
pub fn admit(
    dir: &std::path::Path,
    key: &str,
    record: &OperationRecord,
    fingerprint: &str,
) -> Result<Admission, String> {
    if !record.execution_allowed {
        return Ok(Admission::Refused);
    }
    std::fs::create_dir_all(dir).map_err(|err| format!("create run dir: {err}"))?;
    let mut conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reservations (
            idempotency_key TEXT PRIMARY KEY,
            operation_id TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            run_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            status TEXT NOT NULL
         );",
    )
    .map_err(|err| format!("prepare reservations: {err}"))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("begin reservation: {err}"))?;
    let existing = tx
        .query_row(
            "SELECT run_id, operation_id, fingerprint, workspace_id, status
             FROM reservations WHERE idempotency_key = ?1",
            [key],
            |row| {
                Ok(Reservation {
                    run_id: row.get(0)?,
                    operation_id: row.get(1)?,
                    fingerprint: row.get(2)?,
                    workspace_id: row.get(3)?,
                    status: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|err| format!("read reservation: {err}"))?;
    if let Some(stored) = existing {
        let decision = fingerprint_decision(
            &stored.operation_id,
            &stored.fingerprint,
            &record.operation_id,
            fingerprint,
        );
        tx.commit()
            .map_err(|err| format!("commit reservation: {err}"))?;
        return Ok(match decision {
            FingerprintDecision::Replay => Admission::Existing(stored),
            FingerprintDecision::Conflict | FingerprintDecision::New => Admission::Conflict(stored),
        });
    }
    let run_id = format!("run_{}", &nxr_core::digest_bytes(key.as_bytes())[..16]);
    tx.execute(
        "INSERT INTO reservations
         (idempotency_key, operation_id, fingerprint, run_id, workspace_id, status)
         VALUES (?1, ?2, ?3, ?4, ?5, 'reserved')",
        rusqlite::params![
            key,
            record.operation_id,
            fingerprint,
            run_id,
            record.workspace_id
        ],
    )
    .map_err(|err| format!("insert reservation: {err}"))?;
    tx.commit()
        .map_err(|err| format!("commit reservation: {err}"))?;
    Ok(Admission::Reserved(Reservation {
        run_id,
        operation_id: record.operation_id.clone(),
        fingerprint: fingerprint.to_owned(),
        workspace_id: record.workspace_id.clone(),
        status: "reserved".to_owned(),
    }))
}

/// Put a bound reservation back into `admitting` so a later start can find the owned run.
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn mark_bind_incomplete(dir: &std::path::Path, key: &str) -> Result<(), String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.execute(
        "UPDATE reservations SET status = 'admitting' WHERE idempotency_key = ?1",
        [key],
    )
    .map_err(|err| format!("mark bind incomplete: {err}"))?;
    Ok(())
}

/// Record that admission has started and no process is owned yet.
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn mark_admitting(dir: &std::path::Path, key: &str) -> Result<(), String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.execute(
        "UPDATE reservations SET status = 'admitting'
         WHERE idempotency_key = ?1 AND status = 'reserved'",
        [key],
    )
    .map_err(|err| format!("mark admitting: {err}"))?;
    Ok(())
}

/// Replace the provisional run id with the supervisor-owned id.
///
/// # Errors
///
/// Returns an error when the key is not still reserved.
pub fn bind_supervisor(
    dir: &std::path::Path,
    key: &str,
    supervisor_run_id: &str,
) -> Result<(), String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    let changed = conn
        .execute(
            "UPDATE reservations SET run_id = ?2, status = 'started'
             WHERE idempotency_key = ?1 AND status IN ('reserved', 'admitting')",
            rusqlite::params![key, supervisor_run_id],
        )
        .map_err(|err| format!("bind supervisor: {err}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("reservation was not reserved".to_owned())
    }
}

/// Leave a reserved key unstarted after admission fails.
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn mark_reconciliation(dir: &std::path::Path, key: &str) -> Result<(), String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.execute(
        "UPDATE reservations SET status = 'reconciliation-required'
         WHERE idempotency_key = ?1 AND status IN ('reserved', 'admitting')",
        [key],
    )
    .map_err(|err| format!("mark reconciliation: {err}"))?;
    Ok(())
}

/// Mark a reserved run started. A second call does not create another run.
///
/// # Errors
///
/// Returns an error when the reservation is missing or belongs to another workspace.
pub fn mark_started(dir: &std::path::Path, run_id: &str, workspace_id: &str) -> Result<(), String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    let changed = conn
        .execute(
            "UPDATE reservations SET status = 'started'
             WHERE run_id = ?1 AND workspace_id = ?2 AND status = 'reserved'",
            rusqlite::params![run_id, workspace_id],
        )
        .map_err(|err| format!("mark started: {err}"))?;
    if changed == 0 {
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM reservations WHERE run_id = ?1 AND workspace_id = ?2",
                rusqlite::params![run_id, workspace_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| format!("read start: {err}"))?;
        if status.as_deref() == Some("started") {
            return Ok(());
        }
        return Err("run is not reserved in this workspace".to_owned());
    }
    Ok(())
}

/// Cancel a run only when it belongs to `workspace_id`.
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn cancel(dir: &std::path::Path, run_id: &str, workspace_id: &str) -> Result<bool, String> {
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    let changed = conn
        .execute(
            "UPDATE reservations SET status = 'cancelled'
             WHERE run_id = ?1 AND workspace_id = ?2",
            rusqlite::params![run_id, workspace_id],
        )
        .map_err(|err| format!("cancel reservation: {err}"))?;
    Ok(changed == 1)
}

/// Read one reservation in this workspace.
///
/// # Errors
///
/// Returns an error when the database cannot be read.
pub fn inspect(
    dir: &std::path::Path,
    run_id: &str,
    workspace_id: &str,
) -> Result<Option<Reservation>, String> {
    if !dir.join("idempotency.sqlite").exists() {
        return Ok(None);
    }
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.query_row(
        "SELECT run_id, operation_id, fingerprint, workspace_id, status
         FROM reservations WHERE run_id = ?1 AND workspace_id = ?2",
        rusqlite::params![run_id, workspace_id],
        |row| {
            Ok(Reservation {
                run_id: row.get(0)?,
                operation_id: row.get(1)?,
                fingerprint: row.get(2)?,
                workspace_id: row.get(3)?,
                status: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|err| format!("inspect reservation: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{
        Declaration, OperationKind, WorkspaceInstance, execution_fingerprint, operation_record,
    };

    fn allowed(name: &str) -> OperationRecord {
        operation_record(
            &WorkspaceInstance {
                workspace_id: "lattice".to_owned(),
                workspace_root: "/tmp/lattice".to_owned(),
                source_revision: Some("abc".to_owned()),
            },
            &Declaration::named(OperationKind::Task, name, "flake.nix"),
        )
    }

    #[test]
    fn a_repeated_key_does_not_start_twice_and_a_dirty_file_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let record = allowed("test");
        let first_print = execution_fingerprint(&record, &[("src/lib.rs", b"fn a() {}")]);
        let reserved = admit(dir.path(), "key-1", &record, &first_print).unwrap();
        let Admission::Reserved(run) = &reserved else {
            panic!("expected a new reservation");
        };
        let run_id = run.run_id.clone();
        drop(reserved);
        let again = admit(dir.path(), "key-1", &record, &first_print).unwrap();
        match again {
            Admission::Existing(existing) => assert_eq!(existing.run_id, run_id),
            other => panic!("expected the original run, got {other:?}"),
        }
        let dirty = execution_fingerprint(&record, &[("src/lib.rs", b"fn b() {}")]);
        match admit(dir.path(), "key-1", &record, &dirty).unwrap() {
            Admission::Conflict(existing) => assert_eq!(existing.run_id, run_id),
            other => panic!("expected a conflict, got {other:?}"),
        }
        let blocked = allowed("reset-db");
        assert!(matches!(
            admit(dir.path(), "key-2", &blocked, &first_print).unwrap(),
            Admission::Refused
        ));
        assert!(cancel(dir.path(), &run_id, "other").unwrap() == false);
        assert!(cancel(dir.path(), &run_id, "lattice").unwrap());
    }
}
