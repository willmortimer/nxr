//! Coalesce adjacent child pipe reads before they become task events.
//!
//! Reduces channel/sink overhead for high-output DAGs while preserving
//! ADR-0143 drain-until-`WouldBlock` semantics (coalescing happens after read).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::pipe_multiplex::{PipeChunk, PipeStream};

/// Limits for holding partial stream output before emitting one chunk event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoalesceLimits {
    /// Maximum bytes retained per `(node, stream)` before a flush.
    pub max_bytes: usize,
    /// Maximum complete newline-terminated records per pending buffer.
    pub max_lines: usize,
    /// Maximum hold time so interactive output still appears promptly.
    pub max_latency: Duration,
}

impl Default for CoalesceLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024,
            max_lines: 32,
            max_latency: Duration::from_millis(8),
        }
    }
}

#[derive(Debug)]
struct PendingChunk {
    node: u32,
    stream: PipeStream,
    bytes: Vec<u8>,
    line_count: usize,
    started: Instant,
}

/// Stateful coalescer for [`PipeChunk`] streams keyed by `(node, stream)`.
#[derive(Debug)]
pub struct ChunkCoalescer {
    limits: CoalesceLimits,
    pending: HashMap<(u32, PipeStream), PendingChunk>,
}

impl ChunkCoalescer {
    /// Create a coalescer with the given limits.
    #[must_use]
    pub fn new(limits: CoalesceLimits) -> Self {
        Self {
            limits,
            pending: HashMap::new(),
        }
    }

    /// Ingest one read chunk; returns any chunks ready to emit immediately.
    pub fn push(&mut self, chunk: PipeChunk) -> Vec<PipeChunk> {
        let key = (chunk.node, chunk.stream);
        let mut ready = Vec::new();

        match self.pending.remove(&key) {
            None => {
                let line_count = count_complete_lines(&chunk.bytes);
                let entry = PendingChunk {
                    node: chunk.node,
                    stream: chunk.stream,
                    bytes: chunk.bytes,
                    line_count,
                    started: Instant::now(),
                };
                if should_flush(&entry, self.limits) {
                    ready.push(entry.into_chunk());
                } else {
                    self.pending.insert(key, entry);
                }
            }
            Some(mut entry) => {
                entry.bytes.extend_from_slice(&chunk.bytes);
                entry.line_count += count_complete_lines(&chunk.bytes);
                if should_flush(&entry, self.limits) {
                    ready.push(entry.into_chunk());
                } else {
                    self.pending.insert(key, entry);
                }
            }
        }

        ready
    }

    /// Flush pending buffers whose latency budget has elapsed.
    pub fn flush_expired(&mut self, now: Instant) -> Vec<PipeChunk> {
        let expired: Vec<(u32, PipeStream)> = self
            .pending
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.started) >= self.limits.max_latency)
            .map(|(key, _)| *key)
            .collect();

        expired
            .into_iter()
            .filter_map(|key| self.pending.remove(&key).map(PendingChunk::into_chunk))
            .collect()
    }

    /// Flush every pending buffer (shutdown / final drain).
    pub fn flush_all(&mut self) -> Vec<PipeChunk> {
        self.pending
            .drain()
            .map(|(_, entry)| entry.into_chunk())
            .collect()
    }
}

impl PendingChunk {
    fn into_chunk(self) -> PipeChunk {
        PipeChunk {
            node: self.node,
            stream: self.stream,
            bytes: self.bytes,
        }
    }
}

fn should_flush(entry: &PendingChunk, limits: CoalesceLimits) -> bool {
    entry.bytes.len() >= limits.max_bytes
        || entry.line_count >= limits.max_lines
        || entry.started.elapsed() >= limits.max_latency
}

fn count_complete_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
}

#[cfg(test)]
mod tests {
    use super::{ChunkCoalescer, CoalesceLimits};
    use crate::pipe_multiplex::{PipeChunk, PipeStream};
    use std::thread;
    use std::time::{Duration, Instant};

    fn chunk(node: u32, stream: PipeStream, text: &str) -> PipeChunk {
        PipeChunk {
            node,
            stream,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn merges_adjacent_chunks_from_same_stream() {
        let mut coalescer = ChunkCoalescer::new(CoalesceLimits {
            max_bytes: 1024,
            max_lines: 64,
            max_latency: Duration::from_secs(60),
        });

        assert!(
            coalescer
                .push(chunk(1, PipeStream::Stdout, "hel"))
                .is_empty()
        );
        assert!(
            coalescer
                .push(chunk(1, PipeStream::Stdout, "lo\n"))
                .is_empty()
        );
        let flushed = coalescer.flush_all();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].bytes, b"hello\n");
    }

    #[test]
    fn does_not_merge_different_streams() {
        let mut coalescer = ChunkCoalescer::new(CoalesceLimits {
            max_bytes: 1024,
            max_lines: 64,
            max_latency: Duration::from_millis(1),
        });

        let _ = coalescer.push(chunk(1, PipeStream::Stdout, "out\n"));
        let _ = coalescer.push(chunk(1, PipeStream::Stderr, "err\n"));
        let flushed = coalescer.flush_all();
        assert_eq!(flushed.len(), 2);
    }

    #[test]
    fn flushes_on_byte_limit() {
        let mut coalescer = ChunkCoalescer::new(CoalesceLimits {
            max_bytes: 8,
            max_lines: 64,
            max_latency: Duration::from_secs(60),
        });

        let ready = coalescer.push(chunk(1, PipeStream::Stdout, "12345678"));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].bytes.len(), 8);
    }

    #[test]
    fn flushes_on_line_limit() {
        let mut coalescer = ChunkCoalescer::new(CoalesceLimits {
            max_bytes: 64 * 1024,
            max_lines: 2,
            max_latency: Duration::from_secs(60),
        });

        let _ = coalescer.push(chunk(1, PipeStream::Stdout, "a\n"));
        let ready = coalescer.push(chunk(1, PipeStream::Stdout, "b\nc\n"));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].bytes, b"a\nb\nc\n");
        assert!(coalescer.flush_all().is_empty());
    }

    #[test]
    fn flush_expired_respects_latency_budget() {
        let limits = CoalesceLimits {
            max_bytes: 64 * 1024,
            max_lines: 64,
            max_latency: Duration::from_millis(5),
        };
        let mut coalescer = ChunkCoalescer::new(limits);
        let _ = coalescer.push(chunk(1, PipeStream::Stdout, "tail"));
        thread::sleep(Duration::from_millis(8));
        let ready = coalescer.flush_expired(Instant::now());
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].bytes, b"tail");
    }

    #[test]
    fn flush_all_emits_trailing_partial_lines() {
        let mut coalescer = ChunkCoalescer::new(CoalesceLimits::default());
        let _ = coalescer.push(chunk(2, PipeStream::Stderr, "rapid"));
        let ready = coalescer.flush_all();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].bytes, b"rapid");
    }
}
