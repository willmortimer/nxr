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

/// Idempotency key bound to a supervisor run, when one reservation stores that id.
///
/// # Errors
///
/// Returns an error when the database cannot be read.
pub fn idempotency_key_for_run(
    dir: &std::path::Path,
    run_id: &str,
) -> Result<Option<String>, String> {
    if !dir.join("idempotency.sqlite").exists() {
        return Ok(None);
    }
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    conn.query_row(
        "SELECT idempotency_key FROM reservations WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|err| format!("read idempotency key: {err}"))
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

/// One caller joining or starting a verified execution.
#[derive(Clone, Debug)]
pub struct CoalesceRequest {
    pub observer_id: String,
    pub run_id: String,
    pub identity: crate::coalesce::ExecutionIdentity,
    pub resources: Vec<String>,
    pub verified: bool,
}

/// What the immediate transaction decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoalesceOutcome {
    Start { run_id: String },
    Attach { run_id: String },
    Conflict { reason: String },
    Unverified,
}

/// Result of removing one observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetachOutcome {
    Detach,
    Terminate,
    Absent,
}

/// Attach `observer_id` to an active verified run, or reserve `run_id`.
///
/// # Errors
///
/// Returns an error when the database cannot be locked or written.
pub fn attach_observer(
    dir: &std::path::Path,
    request: &CoalesceRequest,
) -> Result<CoalesceOutcome, String> {
    use crate::coalesce::coalesce_decision;
    std::fs::create_dir_all(dir).map_err(|err| format!("create run dir: {err}"))?;
    let mut conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    ensure_coalesce(&conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("begin coalesce: {err}"))?;
    let active = load_active(&tx)?;
    let decision = coalesce_decision(
        &request.identity,
        request.verified,
        &request.resources,
        &active,
    );
    let outcome = match decision {
        Err(_) => CoalesceOutcome::Unverified,
        Ok(crate::coalesce::CoalesceDecision::Conflict { reason }) => {
            CoalesceOutcome::Conflict { reason }
        }
        Ok(crate::coalesce::CoalesceDecision::Attach { run_id }) => {
            insert_observer(&tx, &run_id, &request.observer_id)?;
            CoalesceOutcome::Attach { run_id }
        }
        Ok(crate::coalesce::CoalesceDecision::Start) => {
            insert_run(&tx, request)?;
            insert_observer(&tx, &request.run_id, &request.observer_id)?;
            CoalesceOutcome::Start {
                run_id: request.run_id.clone(),
            }
        }
    };
    tx.commit()
        .map_err(|err| format!("commit coalesce: {err}"))?;
    Ok(outcome)
}

/// Remove one observer. A second detach of the same id is [`DetachOutcome::Absent`].
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn detach_observer_id(
    dir: &std::path::Path,
    run_id: &str,
    observer_id: &str,
) -> Result<DetachOutcome, String> {
    if !dir.join("idempotency.sqlite").exists() {
        return Ok(DetachOutcome::Absent);
    }
    let mut conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    ensure_coalesce(&conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("begin detach: {err}"))?;
    let removed = tx
        .execute(
            "DELETE FROM coalesce_observers WHERE run_id = ?1 AND observer_id = ?2",
            rusqlite::params![run_id, observer_id],
        )
        .map_err(|err| format!("delete observer: {err}"))?;
    if removed == 0 {
        tx.commit().map_err(|err| format!("commit detach: {err}"))?;
        return Ok(DetachOutcome::Absent);
    }
    let remaining: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM coalesce_observers WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(|err| format!("count observers: {err}"))?;
    if remaining == 0 {
        tx.execute(
            "UPDATE coalesce_runs SET status = 'finished' WHERE run_id = ?1",
            [run_id],
        )
        .map_err(|err| format!("finish coalesce run: {err}"))?;
        tx.commit().map_err(|err| format!("commit detach: {err}"))?;
        return Ok(DetachOutcome::Terminate);
    }
    tx.commit().map_err(|err| format!("commit detach: {err}"))?;
    Ok(DetachOutcome::Detach)
}

/// Mark a reserved run finished when start fails after the transaction.
///
/// # Errors
///
/// Returns an error when the database cannot be updated.
pub fn finish_coalesce(dir: &std::path::Path, run_id: &str) -> Result<(), String> {
    if !dir.join("idempotency.sqlite").exists() {
        return Ok(());
    }
    let conn = Connection::open(dir.join("idempotency.sqlite"))
        .map_err(|err| format!("open idempotency db: {err}"))?;
    ensure_coalesce(&conn)?;
    conn.execute(
        "UPDATE coalesce_runs SET status = 'finished' WHERE run_id = ?1",
        [run_id],
    )
    .map_err(|err| format!("finish coalesce run: {err}"))?;
    Ok(())
}

fn ensure_coalesce(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS coalesce_runs (
            run_id TEXT PRIMARY KEY,
            operation_id TEXT NOT NULL,
            parameters TEXT NOT NULL,
            source_digest TEXT NOT NULL,
            toolchain TEXT NOT NULL,
            platform TEXT NOT NULL,
            env_digest TEXT NOT NULL,
            resources TEXT NOT NULL,
            status TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS coalesce_observers (
            run_id TEXT NOT NULL,
            observer_id TEXT NOT NULL,
            PRIMARY KEY (run_id, observer_id)
         );",
    )
    .map_err(|err| format!("prepare coalesce: {err}"))
}

fn load_active(tx: &rusqlite::Transaction<'_>) -> Result<Vec<crate::coalesce::ActiveRun>, String> {
    let mut stmt = tx
        .prepare(
            "SELECT run_id, operation_id, parameters, source_digest, toolchain, platform, env_digest, resources
             FROM coalesce_runs WHERE status = 'active'",
        )
        .map_err(|err| format!("list active runs: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|err| format!("read active runs: {err}"))?;
    let mut active = Vec::new();
    for row in rows {
        let (
            run_id,
            operation_id,
            parameters,
            source_digest,
            toolchain,
            platform,
            env_digest,
            resources,
        ) = row.map_err(|err| format!("read active run: {err}"))?;
        let observers: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM coalesce_observers WHERE run_id = ?1",
                [&run_id],
                |row| row.get(0),
            )
            .unwrap_or(1);
        let parsed: Vec<String> = serde_json::from_str(&resources).unwrap_or_default();
        active.push(crate::coalesce::ActiveRun {
            run_id,
            identity: crate::coalesce::ExecutionIdentity {
                operation_id,
                parameters,
                source_digest,
                toolchain,
                platform,
                env_digest,
            },
            verified: true,
            resources: parsed,
            observers: u32::try_from(observers).unwrap_or(1),
        });
    }
    Ok(active)
}

fn insert_run(tx: &rusqlite::Transaction<'_>, request: &CoalesceRequest) -> Result<(), String> {
    let resources = serde_json::to_string(&request.resources).unwrap_or_else(|_| "[]".to_owned());
    tx.execute(
        "INSERT INTO coalesce_runs
         (run_id, operation_id, parameters, source_digest, toolchain, platform, env_digest, resources, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active')",
        rusqlite::params![
            request.run_id,
            request.identity.operation_id,
            request.identity.parameters,
            request.identity.source_digest,
            request.identity.toolchain,
            request.identity.platform,
            request.identity.env_digest,
            resources
        ],
    )
    .map_err(|err| format!("insert coalesce run: {err}"))?;
    Ok(())
}

fn insert_observer(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    observer_id: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT OR IGNORE INTO coalesce_observers (run_id, observer_id) VALUES (?1, ?2)",
        rusqlite::params![run_id, observer_id],
    )
    .map_err(|err| format!("insert observer: {err}"))?;
    Ok(())
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
        assert!(!cancel(dir.path(), &run_id, "other").unwrap());
        assert!(cancel(dir.path(), &run_id, "lattice").unwrap());
    }

    fn identity() -> crate::coalesce::ExecutionIdentity {
        crate::coalesce::ExecutionIdentity {
            operation_id: "op".into(),
            parameters: "params".into(),
            source_digest: "source".into(),
            toolchain: "rustc".into(),
            platform: "host".into(),
            env_digest: "env".into(),
        }
    }

    fn request(observer: &str, run: &str) -> CoalesceRequest {
        CoalesceRequest {
            observer_id: observer.into(),
            run_id: run.into(),
            identity: identity(),
            resources: vec!["/tmp/target".into()],
            verified: true,
        }
    }

    #[test]
    fn two_observers_attach_to_one_run_and_the_last_one_terminates() {
        let dir = tempfile::tempdir().unwrap();
        let first = attach_observer(dir.path(), &request("obs-a", "run-a")).unwrap();
        assert_eq!(
            first,
            CoalesceOutcome::Start {
                run_id: "run-a".into()
            }
        );
        let second = attach_observer(dir.path(), &request("obs-b", "run-b")).unwrap();
        assert_eq!(
            second,
            CoalesceOutcome::Attach {
                run_id: "run-a".into()
            }
        );
        assert_eq!(
            detach_observer_id(dir.path(), "run-a", "obs-a").unwrap(),
            DetachOutcome::Detach
        );
        assert_eq!(
            detach_observer_id(dir.path(), "run-a", "obs-a").unwrap(),
            DetachOutcome::Absent
        );
        assert_eq!(
            detach_observer_id(dir.path(), "run-a", "obs-b").unwrap(),
            DetachOutcome::Terminate
        );
    }
}
