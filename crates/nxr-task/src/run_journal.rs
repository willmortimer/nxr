//! Single-writer journal for `run-event-v2`.
//!
//! SQLite assigns the sequence inside an immediate transaction. Log bytes live
//! in chunk files named by `log_ref`. An uncommitted insert disappears on
//! reopen. Replay is paged and does not load the whole history.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

pub const RUN_EVENT_V2: &str = "run-event-v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub schema_version: String,
    pub run_id: String,
    pub operation_id: String,
    pub sequence: u64,
    pub timestamp: String,
    pub event_type: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_ref: Option<String>,
}

pub struct RunJournal {
    dir: PathBuf,
    conn: Connection,
    _lock: File,
}

impl RunJournal {
    /// Open or create the journal and take the single-writer lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory, lock, or database cannot be opened.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|err| format!("create journal dir: {err}"))?;
        fs::create_dir_all(dir.join("logs")).map_err(|err| format!("create log dir: {err}"))?;
        let lock_path = dir.join("writer.lock");
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| format!("open journal lock: {err}"))?;
        lock.try_lock_exclusive()
            .map_err(|_| "journal already has a writer".to_owned())?;
        let conn = Connection::open(dir.join("events.sqlite"))
            .map_err(|err| format!("open journal db: {err}"))?;
        conn.execute_batch(
            "PRAGMA synchronous = FULL;
             PRAGMA journal_mode = DELETE;
             CREATE TABLE IF NOT EXISTS events (
               sequence INTEGER PRIMARY KEY,
               run_id TEXT NOT NULL,
               operation_id TEXT NOT NULL,
               timestamp TEXT NOT NULL,
               event_type TEXT NOT NULL,
               source TEXT NOT NULL,
               exit_code INTEGER,
               log_ref TEXT,
               body TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS log_chunks (
               log_ref TEXT PRIMARY KEY,
               bytes INTEGER NOT NULL
             );",
        )
        .map_err(|err| format!("migrate journal: {err}"))?;
        Ok(Self {
            dir,
            conn,
            _lock: lock,
        })
    }

    /// Highest committed sequence, or zero.
    ///
    /// # Errors
    ///
    /// Returns an error when the index cannot be read.
    pub fn last_sequence(&self) -> Result<u64, String> {
        self.conn
            .query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|value| u64::try_from(value).unwrap_or(0))
            .map_err(|err| format!("read sequence: {err}"))
    }

    /// Append the next event. The sequence must be `last_sequence + 1`.
    ///
    /// # Errors
    ///
    /// Returns an error on a sequence gap or a failed commit.
    pub fn append(&mut self, event: JournalEvent) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| format!("begin append: {err}"))?;
        let last: i64 = tx
            .query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| {
                row.get(0)
            })
            .map_err(|err| format!("read sequence: {err}"))?;
        let expected = u64::try_from(last).unwrap_or(0) + 1;
        if event.sequence != expected {
            return Err(format!(
                "event sequence {} does not follow {expected}",
                event.sequence
            ));
        }
        let body = serde_json::to_string(&event).map_err(|err| err.to_string())?;
        tx.execute(
            "INSERT INTO events (sequence, run_id, operation_id, timestamp, event_type, source, exit_code, log_ref, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                i64::try_from(event.sequence).unwrap_or(i64::MAX),
                event.run_id,
                event.operation_id,
                event.timestamp,
                event.event_type,
                event.source,
                event.exit_code,
                event.log_ref,
                body,
            ],
        )
        .map_err(|err| format!("insert event: {err}"))?;
        tx.commit().map_err(|err| format!("commit event: {err}"))?;
        Ok(())
    }

    /// Store acknowledged log bytes and fsync them before the index row commits.
    ///
    /// # Errors
    ///
    /// Returns an error when the chunk cannot be written or recorded.
    pub fn write_log(&mut self, log_ref: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.log_path(log_ref)?;
        let mut file = File::create(&path).map_err(|err| format!("create log: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("write log: {err}"))?;
        file.sync_all().map_err(|err| format!("sync log: {err}"))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| format!("begin log: {err}"))?;
        tx.execute(
            "INSERT OR REPLACE INTO log_chunks (log_ref, bytes) VALUES (?1, ?2)",
            rusqlite::params![log_ref, i64::try_from(bytes.len()).unwrap_or(i64::MAX)],
        )
        .map_err(|err| format!("index log: {err}"))?;
        tx.commit().map_err(|err| format!("commit log: {err}"))?;
        Ok(())
    }

    /// Read log bytes that were acknowledged in the index.
    ///
    /// # Errors
    ///
    /// Returns an error when the ref was never committed or the file is missing.
    pub fn read_log(&self, log_ref: &str) -> Result<Vec<u8>, String> {
        let indexed: Option<i64> = self
            .conn
            .query_row(
                "SELECT bytes FROM log_chunks WHERE log_ref = ?1",
                [log_ref],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| format!("read log index: {err}"))?;
        if indexed.is_none() {
            return Err(format!("log `{log_ref}` is not acknowledged"));
        }
        fs::read(self.log_path(log_ref)?).map_err(|err| format!("read log: {err}"))
    }

    /// Events with `sequence > after`, at most `limit` rows.
    ///
    /// # Errors
    ///
    /// Returns an error when the page cannot be read.
    pub fn replay_after(&self, after: u64, limit: u64) -> Result<Vec<JournalEvent>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT body FROM events WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2")
            .map_err(|err| format!("prepare replay: {err}"))?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    i64::try_from(after).unwrap_or(0),
                    i64::try_from(limit).unwrap_or(0)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|err| format!("query replay: {err}"))?;
        let mut events = Vec::new();
        for row in rows {
            let body = row.map_err(|err| format!("read replay row: {err}"))?;
            events.push(serde_json::from_str(&body).map_err(|err| format!("parse event: {err}"))?);
        }
        Ok(events)
    }

    /// Insert a row and leave the transaction open so drop rolls it back.
    ///
    /// # Errors
    ///
    /// Returns an error when the uncommitted insert cannot be staged.
    #[cfg(test)]
    pub fn stage_without_commit(&mut self, event: &JournalEvent) -> Result<(), String> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|err| format!("begin torn append: {err}"))?;
        let body = serde_json::to_string(event).map_err(|err| err.to_string())?;
        self.conn
            .execute(
                "INSERT INTO events (sequence, run_id, operation_id, timestamp, event_type, source, exit_code, log_ref, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    i64::try_from(event.sequence).unwrap_or(i64::MAX),
                    event.run_id,
                    event.operation_id,
                    event.timestamp,
                    event.event_type,
                    event.source,
                    event.exit_code,
                    event.log_ref,
                    body,
                ],
            )
            .map_err(|err| format!("stage event: {err}"))?;
        Ok(())
    }

    fn log_path(&self, log_ref: &str) -> Result<PathBuf, String> {
        if log_ref.is_empty() || log_ref.contains('/') || log_ref.contains("..") {
            return Err(format!("log ref `{log_ref}` is not a file name"));
        }
        let name = log_ref.replace(':', "_");
        Ok(self.dir.join("logs").join(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, event_type: &str) -> JournalEvent {
        JournalEvent {
            schema_version: RUN_EVENT_V2.to_owned(),
            run_id: "run_journal".to_owned(),
            operation_id: "op_0123456789abcdef0123456789abcdef".to_owned(),
            sequence,
            timestamp: "2026-10-21T00:00:00Z".to_owned(),
            event_type: event_type.to_owned(),
            source: "local".to_owned(),
            exit_code: None,
            log_ref: None,
        }
    }

    #[test]
    fn uncommitted_append_is_absent_after_reopen() {
        let dir = tempfile::tempdir().expect("temp");
        {
            let mut journal = RunJournal::open(dir.path()).expect("open");
            journal
                .stage_without_commit(&event(1, "started"))
                .expect("stage");
        }
        let journal = RunJournal::open(dir.path()).expect("reopen");
        assert_eq!(journal.last_sequence().expect("sequence"), 0);
        assert!(journal.replay_after(0, 10).expect("replay").is_empty());
    }

    #[test]
    fn replay_is_paged_and_logs_round_trip() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        for sequence in 1..=5 {
            let mut item = event(sequence, "log");
            item.log_ref = Some(format!("log:{sequence}"));
            journal
                .write_log(
                    item.log_ref.as_deref().expect("ref"),
                    format!("bytes-{sequence}").as_bytes(),
                )
                .expect("log");
            journal.append(item).expect("append");
        }
        let page = journal.replay_after(2, 2).expect("page");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].sequence, 3);
        assert_eq!(page[1].sequence, 4);
        let bytes = journal.read_log("log:3").expect("read");
        assert_eq!(bytes, b"bytes-3");
        fs::write(dir.path().join("logs").join("torn"), b"partial").expect("torn");
        let all = journal.replay_after(0, 10).expect("all");
        assert_eq!(all.len(), 5);
        assert!(journal.read_log("torn").is_err());
        let mut cancelled = event(6, "cancelled");
        cancelled.exit_code = None;
        journal.append(cancelled).expect("cancel");
        assert_eq!(
            journal.replay_after(5, 10).expect("tail")[0].event_type,
            "cancelled"
        );
        let duplicate = journal.append(event(6, "started"));
        assert!(duplicate.is_err());
    }
}
