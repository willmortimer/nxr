//! Optional process-log broker retained by `nxrd` ([ADR-0164]).
//!
//! Streams are keyed by opaque client ids (CLI uses `{project_id}/{process}`).
//! Producers push via `log.append`; file-backed followers keep an open FD and
//! stream chunks to subscribers. Retention is a bounded in-memory tail only —
//! the same sensitivity class as on-disk process logs (never a secret store).

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};

/// Kill-switch for CLI broker follow (`off` / `0` / `false` / `no`).
pub const LOG_BROKER_ENV: &str = "NXR_LOG_BROKER";

/// Maximum recent bytes retained per stream in RAM.
pub const MAX_TAIL_BYTES: usize = 256 * 1024;

/// Maximum bytes accepted by a single `log.append`.
pub const MAX_APPEND_BYTES: usize = 64 * 1024;

/// Maximum concurrent streams retained by the daemon.
pub const MAX_STREAMS: usize = 64;

/// File-follow poll interval inside a subscribe handler (ms).
pub const FILE_POLL_MS: u64 = 20;

/// Channel capacity per subscriber (chunk events).
const SUBSCRIBER_CHANNEL_CAP: usize = 64;

/// Whether the CLI may attempt broker-backed `logs --follow`.
///
/// Default: enabled. Disabled when `NXR_LOG_BROKER` is `off` / `0` / `false` /
/// `no`. Independent of [`crate::daemon::DAEMON_ENV`] (which refuses all
/// daemon connects).
#[must_use]
pub fn log_broker_enabled() -> bool {
    log_broker_enabled_for(std::env::var(LOG_BROKER_ENV).ok().as_deref())
}

fn log_broker_enabled_for(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Event delivered to a live subscriber.
#[derive(Clone, Debug)]
pub enum LogEvent {
    /// Raw log bytes (may be a partial line).
    Chunk(Vec<u8>),
    /// Stream closed by producer / `log.close`.
    Closed,
}

struct LogStreamState {
    path: Option<PathBuf>,
    /// Bounded recent bytes (append + file follow share this ring).
    tail: VecDeque<u8>,
    subscribers: Vec<SyncSender<LogEvent>>,
}

impl LogStreamState {
    fn push_tail(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if data.len() >= MAX_TAIL_BYTES {
            self.tail.clear();
            self.tail
                .extend(data[data.len() - MAX_TAIL_BYTES..].iter().copied());
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(data.len())
            .saturating_sub(MAX_TAIL_BYTES);
        if overflow > 0 {
            let _ = self.tail.drain(..overflow.min(self.tail.len()));
        }
        self.tail.extend(data.iter().copied());
    }

    fn fanout(&mut self, event: LogEvent) {
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn tail_bytes(&self) -> Vec<u8> {
        self.tail.iter().copied().collect()
    }
}

/// In-memory multi-stream log broker.
#[derive(Default)]
pub struct LogBroker {
    streams: BTreeMap<String, LogStreamState>,
}

impl std::fmt::Debug for LogBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBroker")
            .field("streams", &self.streams.len())
            .finish()
    }
}

impl LogBroker {
    /// Number of registered streams.
    #[must_use]
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Register or refresh a stream. Optional `path` records the on-disk log
    /// for file-backed follow.
    ///
    /// # Errors
    ///
    /// Returns a message when the stream id is invalid or the map is full.
    pub fn open(&mut self, stream: String, path: Option<PathBuf>) -> Result<(), String> {
        validate_stream_id(&stream)?;
        if let Some(existing) = self.streams.get_mut(&stream) {
            if path.is_some() {
                existing.path = path;
            }
            return Ok(());
        }
        if self.streams.len() >= MAX_STREAMS {
            // Drop the lexicographically first idle stream (no subscribers).
            let victim = self
                .streams
                .iter()
                .find(|(_, state)| state.subscribers.is_empty())
                .map(|(key, _)| key.clone());
            if let Some(key) = victim {
                self.streams.remove(&key);
            } else {
                return Err(format!(
                    "log broker at capacity ({MAX_STREAMS} streams); close a stream first"
                ));
            }
        }
        self.streams.insert(
            stream,
            LogStreamState {
                path,
                tail: VecDeque::new(),
                subscribers: Vec::new(),
            },
        );
        Ok(())
    }

    /// Append bytes to a stream (creates the stream when missing).
    ///
    /// Updates the bounded tail and notifies subscribers. Does **not** write
    /// the filesystem — process supervision owns the log file.
    ///
    /// # Errors
    ///
    /// Returns a message for invalid ids or oversized chunks.
    pub fn append(&mut self, stream: &str, data: &[u8]) -> Result<usize, String> {
        validate_stream_id(stream)?;
        if data.len() > MAX_APPEND_BYTES {
            return Err(format!(
                "log.append chunk exceeds {MAX_APPEND_BYTES} bytes (got {})",
                data.len()
            ));
        }
        if !self.streams.contains_key(stream) {
            self.open(stream.to_owned(), None)?;
        }
        let state = self.streams.get_mut(stream).expect("stream inserted above");
        state.push_tail(data);
        if !data.is_empty() {
            state.fanout(LogEvent::Chunk(data.to_vec()));
        }
        Ok(data.len())
    }

    /// Snapshot the bounded tail and attach a subscriber channel.
    ///
    /// # Errors
    ///
    /// Returns a message when the stream id is invalid.
    pub fn subscribe(&mut self, stream: &str) -> Result<(Vec<u8>, Receiver<LogEvent>), String> {
        validate_stream_id(stream)?;
        if !self.streams.contains_key(stream) {
            self.open(stream.to_owned(), None)?;
        }
        let state = self.streams.get_mut(stream).expect("stream inserted above");
        let (tx, rx) = mpsc::sync_channel(SUBSCRIBER_CHANNEL_CAP);
        state.subscribers.push(tx);
        Ok((state.tail_bytes(), rx))
    }

    /// Path recorded for a stream, when known.
    #[must_use]
    pub fn path_for(&self, stream: &str) -> Option<PathBuf> {
        self.streams.get(stream).and_then(|s| s.path.clone())
    }

    /// Close a stream and notify subscribers.
    pub fn close(&mut self, stream: &str) {
        if let Some(mut state) = self.streams.remove(stream) {
            state.fanout(LogEvent::Closed);
        }
    }
}

fn validate_stream_id(stream: &str) -> Result<(), String> {
    if stream.is_empty() || stream.len() > 256 {
        return Err("stream id must be 1..=256 bytes".to_owned());
    }
    if stream.contains('\0') || stream.contains('\n') {
        return Err("stream id must not contain NUL or newline".to_owned());
    }
    Ok(())
}

/// Encode raw bytes for JSON-lines transport.
#[must_use]
pub fn encode_log_bytes(data: &[u8]) -> String {
    base64_encode(data)
}

/// Decode transport bytes from `log.append` / chunk events.
///
/// # Errors
///
/// Returns a message when the payload is not valid standard base64.
pub fn decode_log_bytes(encoded: &str) -> Result<Vec<u8>, String> {
    base64_decode(encoded)
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0u32, u32::from);
        let b2 = chunk.get(2).copied().map_or(0u32, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 byte {c}")),
        }
    }

    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("invalid base64 length".to_owned());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        let n0 = val(chunk[0])?;
        let n1 = val(chunk[1])?;
        let n2 = if chunk[2] == b'=' { 0 } else { val(chunk[2])? };
        let n3 = if chunk[3] == b'=' { 0 } else { val(chunk[3])? };
        let n =
            (u32::from(n0) << 18) | (u32::from(n1) << 12) | (u32::from(n2) << 6) | u32::from(n3);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad < 1 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_parser() {
        assert!(log_broker_enabled_for(None));
        assert!(!log_broker_enabled_for(Some("off")));
        assert!(!log_broker_enabled_for(Some("0")));
        assert!(!log_broker_enabled_for(Some("false")));
        assert!(!log_broker_enabled_for(Some("no")));
        assert!(log_broker_enabled_for(Some("on")));
    }

    #[test]
    fn append_notifies_subscriber_and_caps_tail() {
        let mut broker = LogBroker::default();
        let (tail, rx) = broker.subscribe("proj/api").expect("subscribe");
        assert!(tail.is_empty());
        broker.append("proj/api", b"hello").expect("append");
        match rx.recv().expect("event") {
            LogEvent::Chunk(data) => assert_eq!(data, b"hello"),
            other => panic!("unexpected {other:?}"),
        }
        let big = vec![b'x'; MAX_TAIL_BYTES + 64];
        broker.append("proj/api", &big[..MAX_APPEND_BYTES]).unwrap();
        // Drain channel; final tail must be capped.
        while rx.try_recv().is_ok() {}
        let (snapshot, _) = broker.subscribe("proj/api").unwrap();
        assert!(snapshot.len() <= MAX_TAIL_BYTES);
    }

    #[test]
    fn base64_round_trip() {
        let samples: &[&[u8]] = &[b"", b"f", b"fo", b"foo", b"foobar", &[0, 1, 2, 255]];
        for sample in samples {
            let encoded = encode_log_bytes(sample);
            let decoded = decode_log_bytes(&encoded).expect("decode");
            assert_eq!(&decoded, sample);
        }
    }

    #[test]
    fn reject_oversized_append() {
        let mut broker = LogBroker::default();
        let huge = vec![0u8; MAX_APPEND_BYTES + 1];
        let err = broker.append("s", &huge).expect_err("too big");
        assert!(err.contains("exceeds"));
    }
}
