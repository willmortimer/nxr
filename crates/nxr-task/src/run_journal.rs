//! Single-writer journal for `run-event-v2`.
//!
//! SQLite assigns the sequence inside an immediate transaction. Log bytes live
//! in chunk files named by `log_ref`. An uncommitted insert disappears on
//! reopen. Replay is paged and does not load the whole history.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

pub const RUN_EVENT_V2: &str = "run-event-v2";

use crate::artifact_ref::{ArtifactRef, VerifiedArtifactRef};
use crate::events::Event;
use crate::run_view::{RunPlanMetadata, RunView};

pub const MAX_LOG_SLICE_BYTES: usize = 64 * 1024;
pub const MAX_FOREGROUND_PAGE: u64 = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// A cursor-addressed log slice. These are bytes, not lossy text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogSlice {
    pub bytes: Vec<u8>,
    pub next_offset: u64,
    pub eof: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForegroundPayload {
    Execution {
        event: Event,
    },
    Log {
        node: String,
        stream: LogStream,
        log_ref: String,
        offset: u64,
        length: u64,
    },
    ArtifactDeclared {
        node: String,
        artifact: ArtifactRef,
    },
    ArtifactVerified {
        node: String,
        artifact: ArtifactRef,
    },
    ArtifactFailure {
        node: String,
        message: String,
    },
    Uncertainty {
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForegroundRecord {
    pub sequence: u64,
    pub source: String,
    pub payload: ForegroundPayload,
}

/// Read-only inspection never takes the writer lock or changes journal state.
pub struct RunJournalReader {
    dir: PathBuf,
    conn: Connection,
}

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
    foreground_lines: std::collections::BTreeMap<(String, String), MarkerLine>,
}

#[derive(Default)]
struct MarkerLine {
    bytes: Vec<u8>,
    oversized: bool,
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
        lock.try_lock_exclusive().map_err(|_| {
            format!(
                "journal already has a writer at {} pid {}",
                lock_path.display(),
                std::process::id()
            )
        })?;
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
               content_hash TEXT NOT NULL,
               bytes INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS foreground_plan (
               singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
               body TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS foreground_events (
               sequence INTEGER PRIMARY KEY,
               body TEXT NOT NULL,
               node TEXT,
               stream TEXT,
               log_ref TEXT,
               log_offset INTEGER,
               bytes INTEGER
             );
             CREATE INDEX IF NOT EXISTS foreground_log_index
               ON foreground_events(node, stream, log_offset);
             CREATE TABLE IF NOT EXISTS foreground_artifacts (
               sequence INTEGER PRIMARY KEY,
               node TEXT NOT NULL,
               body TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS foreground_artifact_node_index
               ON foreground_artifacts(node, sequence);",
        )
        .map_err(|err| format!("migrate journal: {err}"))?;
        Ok(Self {
            dir,
            conn,
            _lock: lock,
            foreground_lines: std::collections::BTreeMap::new(),
        })
    }

    /// Freeze the authoritative DAG once, before recording foreground events.
    pub fn set_foreground_plan(&mut self, plan: &RunPlanMetadata) -> Result<(), String> {
        plan.validate()?;
        let body = serde_json::to_string(plan).map_err(|error| error.to_string())?;
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM foreground_plan WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(existing) = existing {
            if existing != body {
                return Err("foreground plan is immutable".into());
            }
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO foreground_plan(singleton, body) VALUES (1, ?1)",
                [&body],
            )
            .map_err(|error| format!("record foreground plan: {error}"))?;
        Ok(())
    }

    /// Fallible persistence boundary for DAG events. Log bytes are chunked into
    /// separately hashed files before their lightweight event is committed.
    pub fn record_foreground_event(&mut self, event: &Event) -> Result<u64, String> {
        if let Event::ArtifactReused { run_id, .. }
        | Event::ClientDetached { run_id, .. }
        | Event::Phase { run_id, .. } = event
            && run_id != &read_foreground_plan(&self.conn)?.run_id
        {
            return Err("foreground execution evidence belongs to another run".into());
        }
        let (node, stream, bytes) = match event {
            Event::StdoutChunk { node, payload } => (node, LogStream::Stdout, payload.as_bytes()),
            Event::StderrChunk { node, payload } => (node, LogStream::Stderr, payload.as_bytes()),
            _ => {
                match event {
                    Event::NodeQueued { node, .. }
                    | Event::NodeStarted { node, .. }
                    | Event::ArtifactReused { node, .. }
                    | Event::ClientDetached { node, .. }
                    | Event::Phase { node, .. } => self.require_foreground_node(node)?,
                    Event::NodeExited { node, .. } => {
                        self.require_foreground_node(node)?;
                        // EOF completes a final marker even without a newline;
                        // remove per-node line buffers once its pipes close.
                        for stream in [LogStream::Stdout, LogStream::Stderr] {
                            if let Some(pending) = self
                                .foreground_lines
                                .remove(&(node.clone(), stream.as_str().into()))
                                && !pending.oversized
                                && let Some(artifact) = parse_child_artifact(&pending.bytes)
                            {
                                self.record_artifact(node, &artifact)?;
                            }
                        }
                    }
                    Event::PlanCreated { run_id, .. } | Event::RunCompleted { run_id, .. } => {
                        let plan = read_foreground_plan(&self.conn)?;
                        if run_id.as_ref().is_some_and(|id| id != &plan.run_id) {
                            return Err("foreground event belongs to another run".into());
                        }
                    }
                    _ => {}
                }
                return self.append_foreground(
                    ForegroundPayload::Execution {
                        event: event.clone(),
                    },
                    "scheduler",
                );
            }
        };
        self.require_foreground_node(node)?;
        let mut sequence = self.foreground_sequence()?;
        for chunk in bytes.chunks(MAX_LOG_SLICE_BYTES) {
            let offset = nonnegative_query(
                &self.conn,
                "SELECT COALESCE(MAX(log_offset + bytes), 0) FROM foreground_events WHERE node = ?1 AND stream = ?2",
                rusqlite::params![node, stream.as_str()],
            )?;
            let log_ref = format!(
                "foreground:{}:{}",
                self.foreground_sequence()? + 1,
                stream.as_str()
            );
            self.write_log(&log_ref, chunk)?;
            sequence = self.append_foreground(
                ForegroundPayload::Log {
                    node: node.clone(),
                    stream,
                    log_ref,
                    offset,
                    length: chunk.len() as u64,
                },
                "child-output",
            )?;
            let declarations = self.child_artifacts(node, stream, chunk);
            for artifact in declarations {
                sequence = self.record_artifact(node, &artifact)?;
            }
        }
        Ok(sequence)
    }

    /// Child declarations are always unverified even if they contain a claim.
    pub fn record_artifact(&mut self, node: &str, artifact: &ArtifactRef) -> Result<u64, String> {
        self.require_foreground_node(node)?;
        artifact.validate()?;
        let mut artifact = artifact.clone();
        artifact.verified = false;
        self.append_foreground(
            ForegroundPayload::ArtifactDeclared {
                node: node.into(),
                artifact,
            },
            "child-declaration",
        )
    }

    pub fn publish_verified_artifact(
        &mut self,
        node: &str,
        artifact: &VerifiedArtifactRef,
    ) -> Result<u64, String> {
        self.require_foreground_node(node)?;
        self.append_foreground(
            ForegroundPayload::ArtifactVerified {
                node: node.into(),
                artifact: artifact.reference().clone(),
            },
            "artifact-verifier",
        )
    }

    pub fn record_artifact_failure(&mut self, node: &str, message: &str) -> Result<u64, String> {
        self.require_foreground_node(node)?;
        self.append_foreground(
            ForegroundPayload::ArtifactFailure {
                node: node.into(),
                message: message.chars().take(1024).collect(),
            },
            "artifact-verifier",
        )
    }

    pub fn record_uncertainty(&mut self, message: &str) -> Result<u64, String> {
        self.append_foreground(
            ForegroundPayload::Uncertainty {
                message: message.chars().take(1024).collect(),
            },
            "recorder",
        )
    }

    pub fn foreground_sequence(&self) -> Result<u64, String> {
        nonnegative_query(
            &self.conn,
            "SELECT COALESCE(MAX(sequence), 0) FROM foreground_events",
            [],
        )
    }

    fn require_foreground_node(&self, node: &str) -> Result<(), String> {
        let plan = read_foreground_plan(&self.conn)?;
        if !plan.dependencies.contains_key(node) {
            return Err(format!("node {node} is outside the recorded plan"));
        }
        Ok(())
    }

    fn append_foreground(
        &mut self,
        payload: ForegroundPayload,
        source: &str,
    ) -> Result<u64, String> {
        // Requiring the plan before every append prevents orphaned evidence.
        let _ = read_foreground_plan(&self.conn)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let last = nonnegative_query(
            &tx,
            "SELECT COALESCE(MAX(sequence), 0) FROM foreground_events",
            [],
        )?;
        let sequence = last.checked_add(1).ok_or("foreground sequence overflow")?;
        let (node, stream, log_ref, offset, length) = match &payload {
            ForegroundPayload::Log {
                node,
                stream,
                log_ref,
                offset,
                length,
            } => (
                Some(node.as_str()),
                Some(stream.as_str()),
                Some(log_ref.as_str()),
                Some(*offset),
                Some(*length),
            ),
            ForegroundPayload::ArtifactDeclared { node, .. }
            | ForegroundPayload::ArtifactVerified { node, .. }
            | ForegroundPayload::ArtifactFailure { node, .. } => {
                (Some(node.as_str()), None, None, None, None)
            }
            _ => (None, None, None, None, None),
        };
        let record = ForegroundRecord {
            sequence,
            source: source.into(),
            payload: payload.clone(),
        };
        let body = serde_json::to_string(&record).map_err(|error| error.to_string())?;
        let sequence_sql = sqlite_i64(sequence)?;
        let offset_sql = offset.map(sqlite_i64).transpose()?;
        let length_sql = length.map(sqlite_i64).transpose()?;
        tx.execute("INSERT INTO foreground_events(sequence, body, node, stream, log_ref, log_offset, bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![sequence_sql, body, node, stream, log_ref, offset_sql, length_sql]).map_err(|error| format!("append foreground event: {error}"))?;
        if matches!(
            &payload,
            ForegroundPayload::ArtifactDeclared { .. }
                | ForegroundPayload::ArtifactVerified { .. }
                | ForegroundPayload::ArtifactFailure { .. }
        ) {
            tx.execute(
                "INSERT INTO foreground_artifacts(sequence, node, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![sequence_sql, node, body],
            )
            .map_err(|error| format!("index foreground artifact: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("commit foreground event: {error}"))?;
        Ok(sequence)
    }

    fn child_artifacts(&mut self, node: &str, stream: LogStream, bytes: &[u8]) -> Vec<ArtifactRef> {
        let pending = self
            .foreground_lines
            .entry((node.into(), stream.as_str().into()))
            .or_default();
        let mut declarations = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if !pending.oversized
                    && let Some(artifact) = parse_child_artifact(&pending.bytes)
                {
                    declarations.push(artifact);
                }
                pending.bytes.clear();
                pending.oversized = false;
            } else if pending.bytes.len() < MAX_LOG_SLICE_BYTES && !pending.oversized {
                pending.bytes.push(*byte);
            } else {
                pending.bytes.clear();
                pending.oversized = true;
            }
        }
        declarations
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

    /// Store a log chunk once, under its content hash.
    ///
    /// A second write of `log_ref` with different bytes is an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the chunk cannot be written or the ref already differs.
    pub fn write_log(&mut self, log_ref: &str, bytes: &[u8]) -> Result<(), String> {
        let hash = content_hash(bytes);
        if let Some(stored) = self.stored_hash(log_ref)? {
            if stored != hash {
                return Err(format!("log `{log_ref}` already stores different bytes"));
            }
            return Ok(());
        }
        let path = self.chunk_path(&hash)?;
        if path.exists() {
            let existing = fs::read(&path).map_err(|err| format!("read chunk: {err}"))?;
            if existing.as_slice() != bytes {
                return Err(format!("chunk `{hash}` already stores different bytes"));
            }
        } else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|err| format!("create log: {err}"))?;
            file.write_all(bytes)
                .map_err(|err| format!("write log: {err}"))?;
            file.sync_all().map_err(|err| format!("sync log: {err}"))?;
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| format!("begin log: {err}"))?;
        tx.execute(
            "INSERT INTO log_chunks (log_ref, content_hash, bytes) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                log_ref,
                hash,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX)
            ],
        )
        .map_err(|err| format!("index log: {err}"))?;
        tx.commit().map_err(|err| format!("commit log: {err}"))?;
        Ok(())
    }

    /// Read log bytes that were acknowledged in the index.
    ///
    /// # Errors
    ///
    /// Returns an error when the ref was never committed, the file is missing, or the hash differs.
    pub fn read_log(&self, log_ref: &str) -> Result<Vec<u8>, String> {
        let indexed: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT content_hash, bytes FROM log_chunks WHERE log_ref = ?1",
                [log_ref],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|err| format!("read log index: {err}"))?;
        let Some((hash, stored_len)) = indexed else {
            return Err(format!("log `{log_ref}` is not acknowledged"));
        };
        let bytes = fs::read(self.chunk_path(&hash)?).map_err(|err| format!("read log: {err}"))?;
        if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != stored_len
            || content_hash(&bytes) != hash
        {
            return Err(format!("log `{log_ref}` does not match its content hash"));
        }
        Ok(bytes)
    }

    fn stored_hash(&self, log_ref: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT content_hash FROM log_chunks WHERE log_ref = ?1",
                [log_ref],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| format!("read log index: {err}"))
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

    fn chunk_path(&self, hash: &str) -> Result<PathBuf, String> {
        if hash.len() != 64 || !hash.chars().all(|character| character.is_ascii_hexdigit()) {
            return Err(format!("chunk hash `{hash}` is not sha256"));
        }
        Ok(self.dir.join("logs").join(hash))
    }
}

fn parse_child_artifact(bytes: &[u8]) -> Option<ArtifactRef> {
    let text = String::from_utf8_lossy(bytes);
    if text.starts_with("lattice: diagnostic-artifact ") || text.starts_with("nxr: artifact ") {
        ArtifactRef::parse_declaration(&text).ok()
    } else {
        None
    }
}

fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl RunJournalReader {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        let conn = Connection::open_with_flags(
            dir.join("events.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("open run journal for inspection: {error}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        Ok(Self { dir, conn })
    }

    pub fn plan(&self) -> Result<RunPlanMetadata, String> {
        read_foreground_plan(&self.conn)
    }

    pub fn replay_foreground(
        &self,
        after: u64,
        limit: u64,
    ) -> Result<Vec<ForegroundRecord>, String> {
        let mut statement = self.conn.prepare("SELECT sequence, body FROM foreground_events WHERE sequence > ?1 ORDER BY sequence LIMIT ?2").map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    sqlite_i64(after)?,
                    sqlite_i64(limit.min(MAX_FOREGROUND_PAGE))?
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let mut records = Vec::new();
        for row in rows {
            let (sequence, body) = row.map_err(|error| error.to_string())?;
            let sequence = sqlite_u64(sequence)?;
            let record: ForegroundRecord = serde_json::from_str(&body)
                .map_err(|error| format!("corrupt foreground event: {error}"))?;
            if record.sequence != sequence {
                return Err("foreground event cursor does not match its index".into());
            }
            records.push(record);
        }
        Ok(records)
    }

    /// Artifact-only cursor pages use a secondary index and never scan log
    /// events. Declarations, successful verification and failures remain distinct.
    pub fn artifacts(
        &self,
        node: Option<&str>,
        after: u64,
        limit: u64,
    ) -> Result<Vec<ForegroundRecord>, String> {
        let plan = self.plan()?;
        if node.is_some_and(|node| !plan.dependencies.contains_key(node)) {
            return Err("artifact query names a node outside its plan".into());
        }
        let sql = if node.is_some() {
            "SELECT sequence, body FROM foreground_artifacts WHERE sequence > ?1 AND node = ?3 ORDER BY sequence LIMIT ?2"
        } else {
            "SELECT sequence, body FROM foreground_artifacts WHERE sequence > ?1 AND ?3 IS NULL ORDER BY sequence LIMIT ?2"
        };
        let mut statement = self.conn.prepare(sql).map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    sqlite_i64(after)?,
                    sqlite_i64(limit.min(MAX_FOREGROUND_PAGE))?,
                    node
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let mut records = Vec::new();
        for row in rows {
            let (sequence, body) = row.map_err(|error| error.to_string())?;
            let sequence = sqlite_u64(sequence)?;
            let record: ForegroundRecord =
                serde_json::from_str(&body).map_err(|error| error.to_string())?;
            if record.sequence != sequence {
                return Err("artifact cursor does not match its index".into());
            }
            records.push(record);
        }
        Ok(records)
    }

    /// Replay up to the observed high-water mark, so a busy live producer cannot
    /// make an inspection chase an indefinitely growing journal.
    pub fn view(&self) -> Result<RunView, String> {
        let mut view = RunView::new(self.plan()?)?;
        let through = nonnegative_query(
            &self.conn,
            "SELECT COALESCE(MAX(sequence), 0) FROM foreground_events",
            [],
        )?;
        while view.last_sequence < through {
            let records = self.replay_foreground(view.last_sequence, MAX_FOREGROUND_PAGE)?;
            if records.is_empty() {
                return Err("foreground journal ended before its recorded cursor".into());
            }
            for record in records {
                if record.sequence > through {
                    break;
                }
                match record.payload {
                    ForegroundPayload::Execution { event } => {
                        view.apply(record.sequence, &event)?
                    }
                    ForegroundPayload::Log { node, .. } => {
                        view.apply_log(record.sequence, &node)?
                    }
                    ForegroundPayload::ArtifactDeclared { node, artifact } => {
                        view.declare_artifact(record.sequence, &node, &artifact)?
                    }
                    ForegroundPayload::ArtifactVerified { node, artifact } => {
                        view.apply_verified_record(record.sequence, &node, &artifact)?
                    }
                    ForegroundPayload::ArtifactFailure { node, message } => {
                        view.artifact_failure(record.sequence, &node, &message)?
                    }
                    ForegroundPayload::Uncertainty { message } => {
                        view.uncertainty(record.sequence, &message)?
                    }
                }
            }
        }
        let (diagnostics, truncated) = self.diagnostics_through(None, through)?;
        view.diagnostics = diagnostics;
        view.diagnostics_truncated = truncated;
        Ok(view)
    }

    /// Bounded normalized diagnostics from each selected node's latest 64 KiB
    /// per stream. Explicit truncation avoids implying complete log coverage.
    pub fn diagnostics(
        &self,
        node: Option<&str>,
    ) -> Result<(Vec<crate::run_diagnostics::Diagnostic>, bool), String> {
        let through = nonnegative_query(
            &self.conn,
            "SELECT COALESCE(MAX(sequence), 0) FROM foreground_events",
            [],
        )?;
        self.diagnostics_through(node, through)
    }

    fn diagnostics_through(
        &self,
        node: Option<&str>,
        through: u64,
    ) -> Result<(Vec<crate::run_diagnostics::Diagnostic>, bool), String> {
        let plan = self.plan()?;
        if !plan.logs_captured {
            return Ok((Vec::new(), false));
        }
        let selected: Vec<_> = if let Some(node) = node {
            if !plan.dependencies.contains_key(node) {
                return Err(format!("node {node} is outside the recorded plan"));
            }
            if plan
                .uncaptured_nodes
                .iter()
                .any(|uncaptured| uncaptured == node)
            {
                return Err(format!(
                    "node {node} inherited its streams; its output was not captured"
                ));
            }
            vec![node.to_owned()]
        } else {
            plan.dependencies
                .keys()
                .filter(|node| !plan.uncaptured_nodes.contains(node))
                .take(32)
                .cloned()
                .collect()
        };
        let mut truncated = node.is_none() && plan.dependencies.len() > selected.len();
        let mut diagnostics = Vec::new();
        for node in selected {
            for stream in [LogStream::Stderr, LogStream::Stdout] {
                let total = nonnegative_query(
                    &self.conn,
                    "SELECT COALESCE(MAX(log_offset + bytes), 0) FROM foreground_events WHERE node = ?1 AND stream = ?2 AND sequence <= ?3",
                    rusqlite::params![node, stream.as_str(), sqlite_i64(through)?],
                )?;
                let offset = total.saturating_sub(MAX_LOG_SLICE_BYTES as u64);
                truncated |= offset > 0;
                let slice = self.read_node_log(&node, stream, offset, (total - offset) as usize)?;
                for diagnostic in
                    crate::run_diagnostics::parse_run_log_reader(std::io::Cursor::new(&slice.bytes))
                        .map_err(|error| error.to_string())?
                {
                    // Legacy supervisor inspection keeps a first-line fallback
                    // for otherwise unclassified failures. Foreground inspection
                    // also covers successful nodes: ordinary child output must
                    // never become a fabricated error diagnostic.
                    if diagnostic.category == "unknown" {
                        continue;
                    }
                    let mut normalized = diagnostic.normalized(&node);
                    normalized
                        .evidence
                        .push(crate::run_diagnostics::EvidenceRef {
                            kind: "log-slice".into(),
                            reference: format!(
                                "{}:{}:{offset}:{}",
                                node,
                                stream.as_str(),
                                slice.next_offset - offset
                            ),
                        });
                    if !diagnostics
                        .iter()
                        .any(|existing: &crate::run_diagnostics::Diagnostic| {
                            existing.fingerprint == normalized.fingerprint
                        })
                    {
                        if diagnostics.len() == 8 {
                            return Ok((diagnostics, true));
                        }
                        diagnostics.push(normalized);
                    }
                }
            }
        }
        Ok((diagnostics, truncated))
    }

    /// Indexed stream length, with the same plan and capture checks as slices.
    pub fn log_length(&self, node: &str, stream: LogStream) -> Result<u64, String> {
        let plan = self.plan()?;
        if !plan.dependencies.contains_key(node) {
            return Err(format!("node {node} is outside the recorded plan"));
        }
        if !plan.logs_captured {
            return Err("log capture was disabled for this run".into());
        }
        if plan
            .uncaptured_nodes
            .iter()
            .any(|uncaptured| uncaptured == node)
        {
            return Err(format!(
                "node {node} inherited its streams; its output was not captured"
            ));
        }
        nonnegative_query(
            &self.conn,
            "SELECT COALESCE(MAX(log_offset + bytes), 0) FROM foreground_events WHERE node = ?1 AND stream = ?2",
            rusqlite::params![node, stream.as_str()],
        )
    }

    /// Retrieve at most 64 KiB, preserving byte boundaries and stdout/stderr
    /// separation. The offset is within this node's selected stream.
    pub fn read_node_log(
        &self,
        node: &str,
        stream: LogStream,
        offset: u64,
        limit: usize,
    ) -> Result<LogSlice, String> {
        let total = self.log_length(node, stream)?;
        let limit = limit.min(MAX_LOG_SLICE_BYTES);
        if offset >= total || limit == 0 {
            return Ok(LogSlice {
                bytes: Vec::new(),
                next_offset: offset,
                eof: offset >= total,
            });
        }
        let mut statement = self.conn.prepare("SELECT log_ref, log_offset, bytes FROM foreground_events WHERE node = ?1 AND stream = ?2 AND log_offset + bytes > ?3 ORDER BY sequence").map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                rusqlite::params![node, stream.as_str(), sqlite_i64(offset)?],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::with_capacity(limit);
        let mut next_offset = offset;
        for row in rows {
            let (log_ref, start, length) = row.map_err(|error| error.to_string())?;
            let start = sqlite_u64(start)?;
            let length = sqlite_u64(length)?;
            if start > next_offset {
                return Err("node log contains an unreported byte gap".into());
            }
            let chunk = self.read_bounded_chunk(&log_ref, length)?;
            let skip = usize::try_from(next_offset - start).map_err(|error| error.to_string())?;
            if skip > chunk.len() {
                return Err("foreground log chunks overlap inconsistently".into());
            }
            let take = (limit - bytes.len()).min(chunk.len().saturating_sub(skip));
            bytes.extend_from_slice(&chunk[skip..skip + take]);
            next_offset += take as u64;
            if bytes.len() == limit {
                break;
            }
        }
        if bytes.is_empty() && next_offset < total {
            return Err("node log index has no bytes at the requested offset".into());
        }
        Ok(LogSlice {
            bytes,
            next_offset,
            eof: next_offset >= total,
        })
    }

    fn read_bounded_chunk(&self, log_ref: &str, length: u64) -> Result<Vec<u8>, String> {
        if length > MAX_LOG_SLICE_BYTES as u64 {
            return Err("foreground log chunk exceeds its size limit".into());
        }
        let (hash, stored_length): (String, i64) = self
            .conn
            .query_row(
                "SELECT content_hash, bytes FROM log_chunks WHERE log_ref = ?1",
                [log_ref],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let stored_length = sqlite_u64(stored_length)?;
        if stored_length != length
            || hash.len() != 64
            || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("foreground log index is invalid".into());
        }
        let directory = openat::Dir::open(&self.dir).map_err(|error| error.to_string())?;
        let logs = directory
            .sub_dir("logs")
            .map_err(|error| error.to_string())?;
        let metadata = logs
            .metadata(hash.as_str())
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() != length {
            return Err("foreground log chunk is not the indexed regular file".into());
        }
        let file = logs
            .open_file(hash.as_str())
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::with_capacity(length as usize);
        use std::io::Read;
        file.take(MAX_LOG_SLICE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != length || content_hash(&bytes) != hash {
            return Err("foreground log bytes failed digest verification".into());
        }
        Ok(bytes)
    }
}

fn sqlite_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value)
        .map_err(|_| "run cursor or byte offset exceeds SQLite's signed integer range".into())
}

fn sqlite_u64(value: i64) -> Result<u64, String> {
    u64::try_from(value)
        .map_err(|_| "run journal contains a negative cursor, offset or length".into())
}

fn nonnegative_query<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    parameters: P,
) -> Result<u64, String> {
    let value: i64 = conn
        .query_row(sql, parameters, |row| row.get(0))
        .map_err(|error| error.to_string())?;
    sqlite_u64(value)
}

fn read_foreground_plan(conn: &Connection) -> Result<RunPlanMetadata, String> {
    let body: String = conn
        .query_row(
            "SELECT body FROM foreground_plan WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("read foreground plan: {error}"))?;
    let plan: RunPlanMetadata =
        serde_json::from_str(&body).map_err(|error| format!("invalid foreground plan: {error}"))?;
    plan.validate()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_sqlite_numbers_reject_negative_and_unrepresentable_values() {
        assert_eq!(
            sqlite_i64(i64::MAX as u64).expect("largest SQLite integer"),
            i64::MAX
        );
        assert!(sqlite_i64(u64::MAX).is_err());
        assert!(sqlite_u64(-1).is_err());
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        assert!(reader.replay_foreground(u64::MAX, 1).is_err());
        assert!(reader.artifacts(None, u64::MAX, 1).is_err());
        journal.conn.execute("INSERT INTO foreground_events(sequence,body,node,stream,log_ref,log_offset,bytes) VALUES (1,'{}','leaf','stdout','invalid',-20,1)", []).expect("inject invalid offset");
        assert!(reader.log_length("leaf", LogStream::Stdout).is_err());
    }

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

    #[test]
    fn a_log_chunk_cannot_be_replaced() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        let log_ref = format!("sha256:{}", content_hash(b"one"));
        journal.write_log(&log_ref, b"one").expect("first");
        assert!(journal.write_log(&log_ref, b"two").is_err());
        journal.write_log(&log_ref, b"one").expect("same bytes");
        assert_eq!(journal.read_log(&log_ref).expect("read"), b"one");
        let hash = content_hash(b"one");
        fs::write(dir.path().join("logs").join(hash), b"tampered").expect("tamper");
        assert!(journal.read_log(&log_ref).is_err());
    }

    #[test]
    fn a_second_open_reports_the_lock_path() {
        let dir = tempfile::tempdir().expect("temp");
        let _held = RunJournal::open(dir.path()).expect("first");
        let error = match RunJournal::open(dir.path()) {
            Err(error) => error,
            Ok(_) => panic!("second open took the lock"),
        };
        assert!(error.contains("journal already has a writer"));
        assert!(error.contains(&dir.path().join("writer.lock").display().to_string()));
        assert!(error.contains(&std::process::id().to_string()));
    }

    fn foreground_plan() -> RunPlanMetadata {
        RunPlanMetadata {
            schema_version: crate::run_view::RUN_PLAN_V1.into(),
            run_id: "run-foreground".into(),
            operation_id: "operation-test".into(),
            workspace_root: "/test/workspace".into(),
            workspace_id: "workspace-test".into(),
            input_provenance: std::collections::BTreeMap::new(),
            roots: vec!["leaf".into()],
            dependencies: std::collections::BTreeMap::from([("leaf".into(), vec![])]),
            logs_captured: true,
            uncaptured_nodes: Vec::new(),
        }
    }

    #[test]
    fn foreground_reader_works_while_writer_owns_the_journal() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .record_foreground_event(&Event::node_started("leaf"))
            .expect("start");
        let reader = RunJournalReader::open(dir.path()).expect("reader without writer lock");
        assert_eq!(
            reader.view().expect("view").state,
            crate::run_view::RunState::Running
        );
        journal
            .record_foreground_event(&Event::node_exited("leaf", Some(0)))
            .expect("exit");
        journal
            .record_foreground_event(&Event::run_completed(true))
            .expect("terminal");
        assert_eq!(
            reader.view().expect("updated view").state,
            crate::run_view::RunState::Succeeded
        );
    }

    #[test]
    fn foreground_chunks_are_separate_and_slices_preserve_exact_bytes() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        let bytes: Vec<u8> = (0..MAX_LOG_SLICE_BYTES * 3)
            .map(|index| (index % 256) as u8)
            .collect();
        journal
            .record_foreground_event(&Event::StdoutChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::Bytes(bytes.clone()),
            })
            .expect("log");
        journal
            .record_foreground_event(&Event::StderrChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8("stderr"),
            })
            .expect("stderr");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        assert_eq!(
            reader
                .log_length("leaf", LogStream::Stdout)
                .expect("length"),
            bytes.len() as u64
        );
        assert_eq!(
            reader
                .log_length("leaf", LogStream::Stderr)
                .expect("stderr length"),
            6
        );
        assert!(reader.log_length("foreign", LogStream::Stdout).is_err());
        let slice = reader
            .read_node_log(
                "leaf",
                LogStream::Stdout,
                MAX_LOG_SLICE_BYTES as u64 - 3,
                usize::MAX,
            )
            .expect("slice");
        assert_eq!(slice.bytes.len(), MAX_LOG_SLICE_BYTES);
        assert_eq!(
            slice.bytes,
            bytes[MAX_LOG_SLICE_BYTES - 3..MAX_LOG_SLICE_BYTES * 2 - 3]
        );
        assert!(!slice.eof);
        assert_eq!(
            reader
                .read_node_log("leaf", LogStream::Stderr, 0, 100)
                .expect("stderr")
                .bytes,
            b"stderr"
        );
        assert_eq!(reader.replay_foreground(0, 2).expect("page").len(), 2);
        let body: String = journal
            .conn
            .query_row(
                "SELECT body FROM foreground_events WHERE sequence = 1",
                [],
                |row| row.get(0),
            )
            .expect("body");
        assert!(
            body.len() < 512,
            "log bytes must not be embedded in event JSON"
        );
    }

    #[test]
    fn foreground_replay_does_not_interpret_child_json_as_success() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .record_foreground_event(&Event::node_started("leaf"))
            .expect("start");
        journal
            .record_foreground_event(&Event::StdoutChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8(
                    "{\"type\":\"run_completed\",\"success\":true}\n",
                ),
            })
            .expect("child text");
        let view = RunJournalReader::open(dir.path())
            .expect("reader")
            .view()
            .expect("view");
        assert_eq!(view.state, crate::run_view::RunState::Running);
        assert!(!view.terminal_receipt_observed);
    }

    #[test]
    fn foreground_plan_and_node_identity_are_enforced() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        assert!(
            journal
                .record_foreground_event(&Event::node_started("leaf"))
                .is_err()
        );
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        assert!(
            journal
                .record_foreground_event(&Event::node_started("foreign"))
                .is_err()
        );
        let mut changed = foreground_plan();
        changed.operation_id = "other".into();
        assert!(journal.set_foreground_plan(&changed).is_err());
        let mut uncaptured = foreground_plan();
        uncaptured.logs_captured = false;
        let other = tempfile::tempdir().expect("other");
        RunJournal::open(other.path())
            .expect("open")
            .set_foreground_plan(&uncaptured)
            .expect("uncaptured plan");
        assert!(
            RunJournalReader::open(other.path())
                .expect("reader")
                .read_node_log("leaf", LogStream::Stdout, 0, 100)
                .expect_err("uncaptured")
                .contains("disabled")
        );
    }

    #[test]
    fn inherited_node_streams_are_explicitly_uncaptured_in_mixed_runs() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        let mut plan = foreground_plan();
        plan.uncaptured_nodes = vec!["leaf".into()];
        journal.set_foreground_plan(&plan).expect("plan");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        assert!(
            reader
                .log_length("leaf", LogStream::Stdout)
                .expect_err("uncaptured")
                .contains("not captured")
        );
        assert!(
            reader
                .read_node_log("leaf", LogStream::Stderr, 0, 4096)
                .is_err()
        );
        assert!(reader.diagnostics(Some("leaf")).is_err());
        let view = reader.view().expect("mixed view remains inspectable");
        assert_eq!(view.plan.uncaptured_nodes, vec!["leaf"]);
        assert!(view.diagnostics_truncated);
    }

    #[test]
    fn ordinary_success_output_does_not_create_error_diagnostics() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .record_foreground_event(&Event::node_started("leaf"))
            .expect("start");
        journal
            .record_foreground_event(&Event::StdoutChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8("ordinary success data\n"),
            })
            .expect("output");
        journal
            .record_foreground_event(&Event::node_exited("leaf", Some(0)))
            .expect("exit");
        journal
            .record_foreground_event(&Event::run_completed(true))
            .expect("terminal");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        assert!(reader.view().expect("view").diagnostics.is_empty());
        assert_eq!(
            reader
                .read_node_log("leaf", LogStream::Stdout, 0, 4096)
                .expect("log")
                .bytes,
            b"ordinary success data\n"
        );
    }

    #[test]
    fn foreground_artifacts_remain_unverified_until_bytes_are_checked() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().expect("temp");
        let store = tempfile::tempdir().expect("store");
        let digest = format!("{:x}", Sha256::digest(b"recording"));
        let object = format!("objects/{digest}/recording.json");
        let marker = format!(
            "lattice: diagnostic-artifact {{\"schema\":\"lattice.diagnostic.artifact.v1\",\"kind\":\"stargate-recording\",\"sha256\":\"{digest}\",\"object\":\"{object}\"}}\n"
        );
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        for part in marker.as_bytes().chunks(7) {
            journal
                .record_foreground_event(&Event::StderrChunk {
                    node: "leaf".into(),
                    payload: crate::OutputPayload::from_bytes(part.to_vec()),
                })
                .expect("part");
        }
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        let view = reader.view().expect("view");
        let artifact = &view.nodes["leaf"].artifacts[0];
        assert!(!artifact.verified);
        let path = store.path().join(object);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"recording").expect("write");
        journal
            .publish_verified_artifact(
                "leaf",
                &artifact.verify_local_cas(store.path()).expect("verify"),
            )
            .expect("publish");
        assert!(reader.view().expect("verified view").nodes["leaf"].artifacts[0].verified);
        let first = reader.artifacts(Some("leaf"), 0, 1).expect("artifact page");
        assert_eq!(first.len(), 1);
        assert!(matches!(
            first[0].payload,
            ForegroundPayload::ArtifactDeclared { .. }
        ));
        let second = reader
            .artifacts(None, first[0].sequence, 1)
            .expect("next artifact page");
        assert_eq!(second.len(), 1);
        assert!(matches!(
            second[0].payload,
            ForegroundPayload::ArtifactVerified { .. }
        ));
        assert!(reader.artifacts(Some("foreign"), 0, 10).is_err());
    }

    #[test]
    fn foreground_eof_completes_an_artifact_marker_without_newline() {
        let dir = tempfile::tempdir().expect("temp");
        let digest = "a".repeat(64);
        let marker = format!(
            "lattice: diagnostic-artifact {{\"schema\":\"lattice.diagnostic.artifact.v1\",\"kind\":\"stargate-recording\",\"sha256\":\"{digest}\",\"object\":\"objects/{digest}/recording.json\"}}"
        );
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .record_foreground_event(&Event::StderrChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8(marker),
            })
            .expect("marker");
        journal
            .record_foreground_event(&Event::node_exited("leaf", Some(0)))
            .expect("exit");
        assert_eq!(
            RunJournalReader::open(dir.path())
                .expect("reader")
                .artifacts(Some("leaf"), 0, 10)
                .expect("artifacts")
                .len(),
            1
        );
    }

    #[test]
    fn artifact_index_retains_records_beyond_the_bounded_view() {
        use crate::artifact_ref::{ARTIFACT_REF_V1, ArtifactLocation};
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        let count = crate::run_view::MAX_NODE_ARTIFACTS + 5;
        let digest = "a".repeat(64);
        for index in 0..count {
            let artifact = ArtifactRef {
                schema_version: ARTIFACT_REF_V1.into(),
                name: format!("recording-{index}"),
                producer: "test".into(),
                kind: "diagnostic-recording".into(),
                media_type: "application/json".into(),
                digest: format!("sha256:{digest}"),
                location: ArtifactLocation::LocalCas {
                    object: format!("objects/{digest}/recording-{index}.json"),
                },
                verified: false,
            };
            journal.record_artifact("leaf", &artifact).expect("declare");
        }
        journal
            .record_artifact_failure("leaf", "overflow artifact verification failed")
            .expect("failure");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        let view = reader.view().expect("bounded replay");
        assert_eq!(
            view.nodes["leaf"].artifacts.len(),
            crate::run_view::MAX_NODE_ARTIFACTS
        );
        assert!(view.nodes["leaf"].artifacts_truncated);
        assert_eq!(
            view.summary.artifact_failures["leaf"],
            "overflow artifact verification failed"
        );
        let page = reader
            .artifacts(Some("leaf"), 0, MAX_FOREGROUND_PAGE)
            .expect("complete index");
        assert_eq!(page.len(), count + 1);
        assert!(matches!(
            &page.last().expect("failure record").payload,
            ForegroundPayload::ArtifactFailure { .. }
        ));
    }

    #[test]
    fn foreground_persistence_errors_are_returned_before_acknowledgment() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .conn
            .execute("DROP TABLE foreground_events", [])
            .expect("inject storage failure");
        assert!(
            journal
                .record_foreground_event(&Event::node_started("leaf"))
                .is_err()
        );
    }

    #[test]
    fn foreground_tampered_log_and_sequence_gap_fail_closed() {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = RunJournal::open(dir.path()).expect("open");
        journal
            .set_foreground_plan(&foreground_plan())
            .expect("plan");
        journal
            .record_foreground_event(&Event::StdoutChunk {
                node: "leaf".into(),
                payload: crate::OutputPayload::utf8("original"),
            })
            .expect("log");
        let hash = content_hash(b"original");
        std::fs::write(dir.path().join("logs").join(hash), b"tampered").expect("tamper");
        let reader = RunJournalReader::open(dir.path()).expect("reader");
        assert!(
            reader
                .read_node_log("leaf", LogStream::Stdout, 0, 100)
                .is_err()
        );
        journal
            .record_foreground_event(&Event::node_started("leaf"))
            .expect("start");
        journal
            .conn
            .execute("DELETE FROM foreground_events WHERE sequence = 1", [])
            .expect("remove committed row");
        assert!(reader.view().is_err());
    }
}
