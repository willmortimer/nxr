//! Task execution output renderers implementing [`EventSink`].
//!
//! Wired from global `--output` and `--events` for parallel and labeled runs.
//! Human modes decode chunk bytes with an incremental UTF-8 decoder so 4 KiB
//! pipe reads never split multi-byte characters into replacement garbage.

use std::collections::BTreeMap;
use std::io::{self, BufReader, IsTerminal, Seek, SeekFrom, Write};

use clap::ValueEnum;
use nxr_task::{ChunkEncoding, Event, EventSink, NullSink, OutputPayload};
use tempfile::NamedTempFile;

/// Multiplexed stdout/stderr presentation for parallel task runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TaskOutputMode {
    /// Prefix each output line with `[node] ` as chunks arrive.
    Live,
    /// Buffer stdout/stderr per node; flush when the node exits.
    Grouped,
    /// Buffer per node; emit buffered output only on nonzero [`Event::NodeExited`].
    Failures,
    /// One-line status table per node (no multiplexed child logs).
    Summary,
    /// Single foreground child inherits stdio (no pipe multiplexing).
    ///
    /// Conflicts with `-j > 1` and `--events`; handled before the event sink.
    Raw,
}

impl TaskOutputMode {
    /// Modes that require piped child stdio and a renderer.
    #[must_use]
    pub const fn is_multiplexed(self) -> bool {
        matches!(
            self,
            Self::Live | Self::Grouped | Self::Failures | Self::Summary
        )
    }
}

/// Optional machine-readable event stream format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum EventsFormat {
    /// One JSON-encoded [`Event`] per line.
    Jsonl,
}

/// Build a sink that applies the selected output and event stream modes.
///
/// When both options are `None`, returns a [`NullSink`] (no forced labeling).
/// [`TaskOutputMode::Raw`] is not rendered here — callers must inherit stdio.
#[must_use]
pub fn build_task_event_sink<'a>(
    output: Option<TaskOutputMode>,
    events: Option<EventsFormat>,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
) -> TaskEventSink<'a> {
    TaskEventSink::new(output, events, stdout, stderr)
}

/// Composite sink for task output renderers and optional JSONL event logging.
pub struct TaskEventSink<'a> {
    inner: TaskEventSinkInner<'a>,
}

enum TaskEventSinkInner<'a> {
    Null,
    OutputOnly {
        mode: TaskOutputMode,
        stdout: &'a mut dyn Write,
        stderr: &'a mut dyn Write,
        state: TaskOutputRendererState,
    },
    EventsOnly {
        format: EventsFormat,
        writer: &'a mut dyn Write,
    },
    Both {
        mode: TaskOutputMode,
        format: EventsFormat,
        stdout: &'a mut dyn Write,
        stderr: &'a mut dyn Write,
        state: TaskOutputRendererState,
    },
}

impl<'a> TaskEventSink<'a> {
    #[must_use]
    pub fn new(
        output: Option<TaskOutputMode>,
        events: Option<EventsFormat>,
        stdout: &'a mut dyn Write,
        stderr: &'a mut dyn Write,
    ) -> Self {
        let output = output.filter(|mode| mode.is_multiplexed());
        let inner = match (output, events) {
            (None, None) => TaskEventSinkInner::Null,
            (Some(mode), None) => TaskEventSinkInner::OutputOnly {
                mode,
                stdout,
                stderr,
                state: TaskOutputRendererState::default(),
            },
            (None, Some(format)) => TaskEventSinkInner::EventsOnly {
                format,
                writer: stderr,
            },
            (Some(mode), Some(format)) => TaskEventSinkInner::Both {
                mode,
                format,
                stdout,
                stderr,
                state: TaskOutputRendererState::default(),
            },
        };

        Self { inner }
    }
}

impl EventSink for TaskEventSink<'_> {
    fn emit(&mut self, event: Event) {
        match &mut self.inner {
            TaskEventSinkInner::Null => {
                let mut sink = NullSink;
                sink.emit(event);
            }
            TaskEventSinkInner::OutputOnly {
                mode,
                stdout,
                stderr,
                state,
            } => {
                let mut renderer = TaskOutputRenderer::from_state(*mode, stdout, stderr, state);
                renderer.emit(event);
            }
            TaskEventSinkInner::EventsOnly { format, writer } => {
                write_jsonl_event(*writer, *format, &event);
            }
            TaskEventSinkInner::Both {
                mode,
                format,
                stdout,
                stderr,
                state,
            } => {
                {
                    let mut renderer = TaskOutputRenderer::from_state(*mode, stdout, stderr, state);
                    renderer.emit(event.clone());
                }
                write_jsonl_event(stderr, *format, &event);
            }
        }
    }
}

/// In-memory buffered output spills to a temp file once per-stream data exceeds
/// this threshold (grouped and failures modes only).
#[cfg(not(test))]
const BUFFER_SPILL_THRESHOLD: usize = 4 * 1024 * 1024;

#[cfg(test)]
const BUFFER_SPILL_THRESHOLD: usize = 64;

/// Live-mode per-node pending line buffer cap before forcing a flush.
const LIVE_PENDING_CAP: usize = 256 * 1024;

/// Batch terminal writes to reduce lock/write syscall overhead.
const WRITE_BATCH_CAPACITY: usize = 8 * 1024;

struct TaskOutputRenderer<'a> {
    mode: TaskOutputMode,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
    state: &'a mut TaskOutputRendererState,
}

#[derive(Default)]
struct TaskOutputRendererState {
    live_stdout: BTreeMap<String, StreamState>,
    live_stderr: BTreeMap<String, StreamState>,
    grouped: BTreeMap<String, NodeBuffers>,
    /// Compact parallel status (`gh run watch`-style) for live mode on a TTY.
    watch: WatchStatusState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchNodePhase {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
    TimedOut,
}

#[derive(Default)]
struct WatchStatusState {
    nodes: BTreeMap<String, WatchNodePhase>,
    line_open: bool,
}

/// Incremental UTF-8 decode + line pending buffer for one node's stream.
#[derive(Clone, Debug, Default)]
struct StreamState {
    decoder: Utf8StreamDecoder,
    /// Decoded text awaiting a newline (live mode).
    pending: String,
}

#[derive(Debug, Default)]
struct NodeBuffers {
    stdout: SpillableBuffer,
    stderr: SpillableBuffer,
}

/// Batches writes to the underlying writer before flushing.
struct WriteBatch<'a> {
    writer: &'a mut dyn Write,
    buf: Vec<u8>,
}

impl<'a> WriteBatch<'a> {
    fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            writer,
            buf: Vec::with_capacity(WRITE_BATCH_CAPACITY),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.buf.len().saturating_add(bytes.len()) > WRITE_BATCH_CAPACITY {
            self.flush()?;
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn push_str(&mut self, text: &str) -> io::Result<()> {
        self.push(text.as_bytes())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.writer.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }
}

impl Drop for WriteBatch<'_> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Buffered stream data held in memory until [`BUFFER_SPILL_THRESHOLD`], then
/// appended to a temp file so grouped/failures modes stay bounded.
#[derive(Debug, Default)]
struct SpillableBuffer {
    memory: Vec<u8>,
    spill: Option<NamedTempFile>,
    ends_with_newline: bool,
}

impl SpillableBuffer {
    fn push_bytes(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        if self.spill.is_some() {
            if let Some(file) = self.spill.as_mut() {
                let _ = file.write_all(chunk);
                let _ = file.flush();
            }
            self.ends_with_newline = chunk.last() == Some(&b'\n');
            return;
        }

        if self.memory.len().saturating_add(chunk.len()) > BUFFER_SPILL_THRESHOLD {
            if let Ok(mut file) = NamedTempFile::new() {
                let _ = file.write_all(&self.memory);
                let _ = file.write_all(chunk);
                let _ = file.flush();
                self.memory.clear();
                self.spill = Some(file);
                self.ends_with_newline = chunk.last() == Some(&b'\n');
            } else {
                self.memory.extend_from_slice(chunk);
                self.ends_with_newline = self.memory.last() == Some(&b'\n');
            }
            return;
        }

        self.memory.extend_from_slice(chunk);
        self.ends_with_newline = self.memory.last() == Some(&b'\n');
    }

    fn write_to(&self, writer: &mut dyn Write) -> io::Result<()> {
        if let Some(file) = &self.spill {
            let mut reader = BufReader::new(file.as_file());
            reader.seek(SeekFrom::Start(0))?;
            io::copy(&mut reader, writer)?;
        }
        if !self.memory.is_empty() {
            writer.write_all(&self.memory)?;
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.memory.is_empty() && self.spill.is_none()
    }

    fn ends_with_newline(&self) -> bool {
        self.ends_with_newline
    }
}

impl<'a> TaskOutputRenderer<'a> {
    fn from_state(
        mode: TaskOutputMode,
        stdout: &'a mut dyn Write,
        stderr: &'a mut dyn Write,
        state: &'a mut TaskOutputRendererState,
    ) -> Self {
        Self {
            mode,
            stdout,
            stderr,
            state,
        }
    }

    fn watch_enabled(&self) -> bool {
        matches!(self.mode, TaskOutputMode::Live) && io::stderr().is_terminal()
    }

    fn clear_status_line(&mut self) {
        if !self.state.watch.line_open {
            return;
        }
        let _ = write!(self.stderr, "\r{:<96}\r", "");
        let _ = self.stderr.flush();
        self.state.watch.line_open = false;
    }

    fn redraw_status_line(&mut self) {
        if !self.watch_enabled() || self.state.watch.nodes.is_empty() {
            return;
        }
        let mut queued = 0usize;
        let mut running = 0usize;
        let mut done = 0usize;
        let mut failed = 0usize;
        for phase in self.state.watch.nodes.values() {
            match phase {
                WatchNodePhase::Queued => queued += 1,
                WatchNodePhase::Running => running += 1,
                WatchNodePhase::Succeeded => done += 1,
                WatchNodePhase::Failed
                | WatchNodePhase::TimedOut
                | WatchNodePhase::Cancelled
                | WatchNodePhase::Skipped => failed += 1,
            }
        }
        let line =
            format!("\r[nxr] {running} running · {queued} queued · {done} done · {failed} failed");
        let _ = write!(self.stderr, "{line:<96}");
        let _ = self.stderr.flush();
        self.state.watch.line_open = true;
    }

    fn set_watch_phase(&mut self, node: &str, phase: WatchNodePhase) {
        if !matches!(self.mode, TaskOutputMode::Live) {
            return;
        }
        self.state.watch.nodes.insert(node.to_owned(), phase);
        self.redraw_status_line();
    }

    fn ingest_live(&mut self, is_stdout: bool, node: &str, payload: &OutputPayload) {
        self.clear_status_line();
        let map = if is_stdout {
            &mut self.state.live_stdout
        } else {
            &mut self.state.live_stderr
        };
        let state = map.entry(node.to_owned()).or_default();
        let writer = if is_stdout {
            &mut *self.stdout
        } else {
            &mut *self.stderr
        };

        if payload.encoding() == ChunkEncoding::Utf8
            && payload
                .as_bytes()
                .iter()
                .all(|byte| is_live_raw_byte(*byte))
        {
            let mut batch = WriteBatch::new(writer);
            write_labeled_bytes(&mut batch, node, payload.as_bytes(), &mut state.pending);
            return;
        }

        let decoded = state.decoder.push(payload.as_bytes());
        let mut batch = WriteBatch::new(writer);
        let _ = write_labeled_lines(&mut batch, node, &decoded, &mut state.pending);
    }

    fn ingest_buffered(&mut self, is_stdout: bool, node: &str, payload: &OutputPayload) {
        let entry = self.state.grouped.entry(node.to_owned()).or_default();
        if is_stdout {
            entry.stdout.push_bytes(payload.as_bytes());
        } else {
            entry.stderr.push_bytes(payload.as_bytes());
        }
    }

    fn flush_live_partial(&mut self, node: &str) {
        self.clear_status_line();
        flush_stream_on_exit(self.stdout, node, &mut self.state.live_stdout);
        flush_stream_on_exit(self.stderr, node, &mut self.state.live_stderr);
    }
}

fn flush_stream_on_exit(
    writer: &mut dyn Write,
    node: &str,
    streams: &mut BTreeMap<String, StreamState>,
) {
    if let Some(mut state) = streams.remove(node) {
        let rest = state.decoder.finish();
        let mut batch = WriteBatch::new(writer);
        if !rest.is_empty() {
            state.pending.push_str(&rest);
        }
        if !state.pending.is_empty() {
            let prefix = format!("[{node}] ");
            let _ = batch.push_str(&prefix);
            let _ = batch.push_str(&state.pending);
            let _ = batch.push(b"\n");
        }
    }
}

impl EventSink for TaskOutputRenderer<'_> {
    fn emit(&mut self, event: Event) {
        match event {
            Event::StdoutChunk { node, payload } => match self.mode {
                TaskOutputMode::Live => self.ingest_live(true, &node, &payload),
                TaskOutputMode::Grouped | TaskOutputMode::Failures => {
                    self.ingest_buffered(true, &node, &payload);
                }
                TaskOutputMode::Raw | TaskOutputMode::Summary => {}
            },
            Event::StderrChunk { node, payload } => match self.mode {
                TaskOutputMode::Live => self.ingest_live(false, &node, &payload),
                TaskOutputMode::Grouped | TaskOutputMode::Failures => {
                    self.ingest_buffered(false, &node, &payload);
                }
                TaskOutputMode::Raw | TaskOutputMode::Summary => {}
            },
            Event::NodeExited {
                node,
                code,
                status,
                duration_ms,
                ..
            } => {
                if matches!(self.mode, TaskOutputMode::Live) {
                    self.flush_live_partial(&node);
                }

                let should_flush = match self.mode {
                    TaskOutputMode::Live | TaskOutputMode::Raw | TaskOutputMode::Summary => false,
                    TaskOutputMode::Grouped => true,
                    TaskOutputMode::Failures => node_failed(code),
                };

                if should_flush {
                    if let Some(buffers) = self.state.grouped.remove(&node) {
                        let _ = write_buffered_output(self.stdout, self.stderr, &buffers);
                    }
                } else if matches!(self.mode, TaskOutputMode::Failures) {
                    let _ = self.state.grouped.remove(&node);
                }

                if matches!(self.mode, TaskOutputMode::Summary) {
                    let status = match status {
                        Some(nxr_task::NodeOutcome::Succeeded) => "succeeded",
                        Some(nxr_task::NodeOutcome::Failed) => "failed",
                        Some(nxr_task::NodeOutcome::Cancelled) => "cancelled",
                        Some(nxr_task::NodeOutcome::Skipped) => "skipped",
                        Some(nxr_task::NodeOutcome::TimedOut) => "timed_out",
                        None => match code {
                            Some(0) => "succeeded",
                            _ => "failed",
                        },
                    };
                    let duration = duration_ms.map_or_else(
                        || "-".to_owned(),
                        |ms| nxr_task::format_duration(std::time::Duration::from_millis(ms)),
                    );
                    let _ = writeln!(self.stdout, "{node:<24} {status:<10} {duration}");
                }

                let phase = match status {
                    Some(nxr_task::NodeOutcome::Succeeded) => WatchNodePhase::Succeeded,
                    Some(nxr_task::NodeOutcome::Failed) => WatchNodePhase::Failed,
                    Some(nxr_task::NodeOutcome::Cancelled) => WatchNodePhase::Cancelled,
                    Some(nxr_task::NodeOutcome::Skipped) => WatchNodePhase::Skipped,
                    Some(nxr_task::NodeOutcome::TimedOut) => WatchNodePhase::TimedOut,
                    None => {
                        if matches!(code, Some(0)) {
                            WatchNodePhase::Succeeded
                        } else {
                            WatchNodePhase::Failed
                        }
                    }
                };
                self.set_watch_phase(&node, phase);
            }
            Event::Diagnostic { message } => {
                self.clear_status_line();
                let _ = writeln!(self.stderr, "{message}");
            }
            Event::PlanCreated { .. } => {
                if matches!(self.mode, TaskOutputMode::Summary) {
                    let _ = writeln!(self.stdout, "{:<24} {:<10} DURATION", "TASK", "STATUS");
                }
            }
            Event::NodeQueued { node, .. } => {
                self.set_watch_phase(&node, WatchNodePhase::Queued);
            }
            Event::NodeStarted { node, .. } => {
                self.set_watch_phase(&node, WatchNodePhase::Running);
            }
            Event::RunCompleted { .. } => {
                self.clear_status_line();
            }
        }
    }
}

/// Incremental UTF-8 decoder that never splits a multi-byte character across
/// chunk boundaries into replacement characters.
#[derive(Clone, Debug, Default)]
pub struct Utf8StreamDecoder {
    /// Incomplete trailing bytes from the previous chunk.
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// Feed raw bytes; return newly completed UTF-8 text.
    ///
    /// Incomplete trailing sequences are held until a later [`push`] or
    /// [`finish`]. Definitely-invalid sequences become U+FFFD.
    pub fn push(&mut self, chunk: &[u8]) -> String {
        if chunk.is_empty() && self.pending.is_empty() {
            return String::new();
        }

        self.pending.extend_from_slice(chunk);
        let mut out = String::new();

        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid])
                                .expect("valid_up_to marks a UTF-8 prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    match err.error_len() {
                        None => {
                            // Incomplete multi-byte sequence at end — hold bytes.
                            break;
                        }
                        Some(len) => {
                            out.push('\u{FFFD}');
                            let drain = len.min(self.pending.len());
                            self.pending.drain(..drain);
                        }
                    }
                }
            }
        }

        out
    }

    /// Flush any remaining bytes (incomplete sequences become U+FFFD).
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let rest = std::mem::take(&mut self.pending);
        String::from_utf8_lossy(&rest).into_owned()
    }
}

#[cfg(test)]
struct JsonlEventsWriter<'a> {
    writer: &'a mut dyn Write,
}

#[cfg(test)]
impl<'a> JsonlEventsWriter<'a> {
    fn new(writer: &'a mut dyn Write) -> Self {
        Self { writer }
    }
}

#[cfg(test)]
impl EventSink for JsonlEventsWriter<'_> {
    fn emit(&mut self, event: Event) {
        write_jsonl_event(self.writer, EventsFormat::Jsonl, &event);
    }
}

fn write_jsonl_event(writer: &mut dyn Write, format: EventsFormat, event: &Event) {
    match format {
        EventsFormat::Jsonl => {
            if let Ok(line) = serde_json::to_string(event) {
                let _ = writeln!(writer, "{line}");
            }
        }
    }
}

fn node_failed(code: Option<i32>) -> bool {
    !matches!(code, Some(0))
}

fn write_buffered_output(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    buffers: &NodeBuffers,
) -> io::Result<()> {
    if !buffers.stdout.is_empty() {
        buffers.stdout.write_to(stdout)?;
        if !buffers.stdout.ends_with_newline() {
            stdout.write_all(b"\n")?;
        }
    }
    if !buffers.stderr.is_empty() {
        buffers.stderr.write_to(stderr)?;
        if !buffers.stderr.ends_with_newline() {
            stderr.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn write_labeled_lines(
    batch: &mut WriteBatch<'_>,
    node: &str,
    text: &str,
    pending: &mut String,
) -> io::Result<()> {
    pending.push_str(text);
    if pending.len() > LIVE_PENDING_CAP {
        let prefix = format!("[{node}] ");
        batch.push_str(&prefix)?;
        batch.push_str(pending)?;
        batch.push(b"\n")?;
        pending.clear();
        return Ok(());
    }

    let prefix = format!("[{node}] ");
    while let Some(newline_idx) = pending.find('\n') {
        let line = pending.drain(..=newline_idx).collect::<String>();
        let line = line.strip_suffix('\n').unwrap_or(&line);
        batch.push_str(&prefix)?;
        batch.push_str(line)?;
        batch.push(b"\n")?;
    }
    Ok(())
}

fn write_labeled_bytes(batch: &mut WriteBatch<'_>, node: &str, text: &[u8], pending: &mut String) {
    if let Ok(chunk) = std::str::from_utf8(text) {
        let _ = write_labeled_lines(batch, node, chunk, pending);
        return;
    }
    let decoded = String::from_utf8_lossy(text);
    let _ = write_labeled_lines(batch, node, &decoded, pending);
}

fn is_live_raw_byte(byte: u8) -> bool {
    byte == b'\n' || byte == b'\t' || byte == b'\r' || (0x20..0x7f).contains(&byte) || byte >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::RecordingSink;

    fn sample_parallel_events() -> Vec<Event> {
        vec![
            Event::StdoutChunk {
                node: "api".to_owned(),
                payload: OutputPayload::utf8("listening\n"),
            },
            Event::StdoutChunk {
                node: "web".to_owned(),
                payload: OutputPayload::utf8("ready"),
            },
            Event::StdoutChunk {
                node: "web".to_owned(),
                payload: OutputPayload::utf8(" in 421 ms\n"),
            },
            Event::StderrChunk {
                node: "worker".to_owned(),
                payload: OutputPayload::utf8("warn: retry\n"),
            },
            Event::node_exited("api".to_owned(), Some(0)),
            Event::node_exited("web".to_owned(), Some(0)),
            Event::node_exited("worker".to_owned(), Some(1)),
        ]
    }

    fn render_output(mode: TaskOutputMode, events: &[Event]) -> (String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut state = TaskOutputRendererState::default();
        let mut sink = TaskOutputRenderer::from_state(mode, &mut stdout, &mut stderr, &mut state);
        for event in events {
            sink.emit(event.clone());
        }
        (
            String::from_utf8(stdout).expect("utf-8 stdout"),
            String::from_utf8(stderr).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn live_mode_renders_coalesced_chunk_events() {
        let events = vec![
            Event::StdoutChunk {
                node: "api".to_owned(),
                payload: OutputPayload::utf8("line1\nline2\n"),
            },
            Event::StdoutChunk {
                node: "api".to_owned(),
                payload: OutputPayload::utf8("line3\n"),
            },
            Event::node_exited("api".to_owned(), Some(0)),
        ];
        let (stdout, _) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, "[api] line1\n[api] line2\n[api] line3\n");
    }

    #[test]
    fn grouped_mode_buffers_coalesced_byte_chunks() {
        let events = vec![
            Event::StdoutChunk {
                node: "big".to_owned(),
                payload: OutputPayload::from_bytes(b"part1".to_vec()),
            },
            Event::StdoutChunk {
                node: "big".to_owned(),
                payload: OutputPayload::from_bytes(b"part2\n".to_vec()),
            },
            Event::node_exited("big".to_owned(), Some(0)),
        ];
        let (stdout, _) = render_output(TaskOutputMode::Grouped, &events);
        assert_eq!(stdout, "part1part2\n");
    }

    #[test]
    fn live_mode_prefixes_each_line() {
        let (stdout, stderr) = render_output(TaskOutputMode::Live, &sample_parallel_events());
        assert_eq!(stdout, "[api] listening\n[web] ready in 421 ms\n");
        assert_eq!(stderr, "[worker] warn: retry\n");
    }

    #[test]
    fn live_mode_flushes_partial_line_on_exit() {
        let events = vec![
            Event::StdoutChunk {
                node: "api".to_owned(),
                payload: OutputPayload::utf8("partial"),
            },
            Event::node_exited("api".to_owned(), Some(0)),
        ];
        let (stdout, stderr) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, "[api] partial\n");
        assert_eq!(stderr, "");
    }

    #[test]
    fn live_mode_no_trailing_newline_still_flushes() {
        let events = vec![
            Event::StdoutChunk {
                node: "api".to_owned(),
                payload: OutputPayload::utf8("no-nl"),
            },
            Event::node_exited("api".to_owned(), Some(0)),
        ];
        let (stdout, _) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, "[api] no-nl\n");
    }

    #[test]
    fn live_mode_preserves_ansi_sequences() {
        let events = vec![
            Event::StdoutChunk {
                node: "t".to_owned(),
                payload: OutputPayload::utf8("\u{1b}[31mred\u{1b}[0m\n"),
            },
            Event::node_exited("t".to_owned(), Some(0)),
        ];
        let (stdout, _) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, "[t] \u{1b}[31mred\u{1b}[0m\n");
    }

    #[test]
    fn live_mode_handles_long_lines() {
        let long = "x".repeat(16_384);
        let events = vec![
            Event::StdoutChunk {
                node: "t".to_owned(),
                payload: OutputPayload::utf8(format!("{long}\n")),
            },
            Event::node_exited("t".to_owned(), Some(0)),
        ];
        let (stdout, _) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, format!("[t] {long}\n"));
    }

    #[test]
    fn grouped_mode_flushes_on_node_exit() {
        let (stdout, stderr) = render_output(TaskOutputMode::Grouped, &sample_parallel_events());
        assert_eq!(stdout, "listening\nready in 421 ms\n");
        assert_eq!(stderr, "warn: retry\n");
    }

    #[test]
    fn failures_mode_emits_only_failed_nodes() {
        let events = vec![
            Event::StdoutChunk {
                node: "ok".to_owned(),
                payload: OutputPayload::utf8("hidden\n"),
            },
            Event::StdoutChunk {
                node: "bad".to_owned(),
                payload: OutputPayload::utf8("boom\n"),
            },
            Event::node_exited("ok".to_owned(), Some(0)),
            Event::node_exited("bad".to_owned(), Some(2)),
        ];
        let (stdout, stderr) = render_output(TaskOutputMode::Failures, &events);
        assert_eq!(stdout, "boom\n");
        assert_eq!(stderr, "");
    }

    #[test]
    fn failures_mode_treats_missing_code_as_failure() {
        let events = vec![
            Event::StderrChunk {
                node: "sig".to_owned(),
                payload: OutputPayload::utf8("killed\n"),
            },
            Event::node_exited("sig".to_owned(), None),
        ];
        let (stdout, stderr) = render_output(TaskOutputMode::Failures, &events);
        assert_eq!(stdout, "");
        assert_eq!(stderr, "killed\n");
    }

    #[test]
    fn utf8_split_at_every_byte_boundary_round_trips() {
        // "✓ café 日本語" — multi-byte sequences across Latin + CJK.
        let text = "✓ café 日本語";
        let bytes = text.as_bytes();

        for split_at in 0..=bytes.len() {
            let mut decoder = Utf8StreamDecoder::default();
            let mut out = String::new();
            out.push_str(&decoder.push(&bytes[..split_at]));
            out.push_str(&decoder.push(&bytes[split_at..]));
            out.push_str(&decoder.finish());
            assert_eq!(out, text, "failed at split_at={split_at}");
        }

        // Also feed one byte at a time through the live renderer.
        let mut events = Vec::new();
        for byte in bytes {
            events.push(Event::StdoutChunk {
                node: "n".to_owned(),
                payload: OutputPayload::from_bytes(vec![*byte]),
            });
        }
        events.push(Event::StdoutChunk {
            node: "n".to_owned(),
            payload: OutputPayload::utf8("\n"),
        });
        events.push(Event::node_exited("n".to_owned(), Some(0)));
        let (stdout, _) = render_output(TaskOutputMode::Live, &events);
        assert_eq!(stdout, format!("[n] {text}\n"));
    }

    #[test]
    fn binary_bytes_survive_jsonl_and_decoder_replaces_invalid() {
        let raw = vec![0x00, 0xff, 0xfe, b'A'];
        let event = Event::StdoutChunk {
            node: "bin".to_owned(),
            payload: OutputPayload::from_bytes(raw.clone()),
        };
        let mut stderr = Vec::new();
        {
            let mut sink = JsonlEventsWriter::new(&mut stderr);
            sink.emit(event.clone());
        }
        let line = String::from_utf8(stderr).expect("utf-8");
        assert!(line.contains("\"encoding\":\"base64\""));
        let parsed: Event = serde_json::from_str(line.trim()).expect("parse jsonl");
        match parsed {
            Event::StdoutChunk { payload, .. } => assert_eq!(payload.as_bytes(), raw.as_slice()),
            other => panic!("unexpected: {other:?}"),
        }

        // Human path: invalid bytes become U+FFFD, never panic.
        let (stdout, _) = render_output(
            TaskOutputMode::Live,
            &[event, Event::node_exited("bin".to_owned(), Some(0))],
        );
        assert!(stdout.starts_with("[bin] "));
        assert!(stdout.contains('A'));
    }

    #[test]
    fn jsonl_events_writer_emits_one_line_per_event() {
        let events = vec![
            Event::node_started("fmt".to_owned()),
            Event::run_completed(true),
        ];
        let mut stderr = Vec::new();
        let mut sink = JsonlEventsWriter::new(&mut stderr);
        for event in &events {
            sink.emit(event.clone());
        }
        let rendered = String::from_utf8(stderr).expect("utf-8");
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"type\":\"node_started\""));
        assert!(lines[1].contains("\"type\":\"run_completed\""));
    }

    #[test]
    fn composite_sink_with_output_and_events_uses_supplied_stderr() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut sink = build_task_event_sink(
            Some(TaskOutputMode::Grouped),
            Some(EventsFormat::Jsonl),
            &mut stdout,
            &mut stderr,
        );
        sink.emit(Event::node_started("fmt".to_owned()));
        sink.emit(Event::StdoutChunk {
            node: "fmt".to_owned(),
            payload: OutputPayload::utf8("ok\n"),
        });
        sink.emit(Event::node_exited("fmt".to_owned(), Some(0)));
        drop(sink);
        assert_eq!(String::from_utf8(stdout).expect("utf-8"), "ok\n");
        let events = String::from_utf8(stderr).expect("utf-8");
        assert!(events.contains("\"type\":\"node_started\""));
    }

    #[test]
    fn grouped_mode_spills_large_buffers_to_temp_files() {
        let chunk = "x".repeat(BUFFER_SPILL_THRESHOLD);
        let events = vec![
            Event::StdoutChunk {
                node: "big".to_owned(),
                payload: OutputPayload::utf8(format!("{chunk}\n")),
            },
            Event::node_exited("big".to_owned(), Some(0)),
        ];
        let (stdout, stderr) = render_output(TaskOutputMode::Grouped, &events);
        assert_eq!(stdout, format!("{chunk}\n"));
        assert_eq!(stderr, "");
    }

    #[test]
    fn composite_sink_with_no_options_is_inert() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut sink = build_task_event_sink(None, None, &mut stdout, &mut stderr);
        sink.emit(Event::StdoutChunk {
            node: "api".to_owned(),
            payload: OutputPayload::utf8("ignored\n"),
        });
        drop(sink);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn raw_mode_is_not_multiplexed() {
        assert!(!TaskOutputMode::Raw.is_multiplexed());
        assert!(TaskOutputMode::Live.is_multiplexed());
    }

    #[test]
    fn summary_mode_prints_header_and_all_outcomes() {
        let events = vec![
            Event::plan_created("root", None, 3),
            Event::NodeExited {
                node: "test".to_owned(),
                code: Some(1),
                status: Some(nxr_task::NodeOutcome::Failed),
                duration_ms: Some(19_400),
                started_at: None,
                finished_at: None,
                reason: None,
                seq: None,
            },
            Event::NodeExited {
                node: "package".to_owned(),
                code: None,
                status: Some(nxr_task::NodeOutcome::Skipped),
                duration_ms: None,
                started_at: None,
                finished_at: None,
                reason: Some("dependency_failed".to_owned()),
                seq: None,
            },
            Event::NodeExited {
                node: "deploy".to_owned(),
                code: None,
                status: Some(nxr_task::NodeOutcome::Cancelled),
                duration_ms: None,
                started_at: None,
                finished_at: None,
                reason: Some("fail_fast".to_owned()),
                seq: None,
            },
        ];
        let (stdout, _) = render_output(TaskOutputMode::Summary, &events);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines[0],
            format!("{:<24} {:<10} DURATION", "TASK", "STATUS")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("test") && line.contains("failed"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("package") && line.contains("skipped"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("deploy") && line.contains("cancelled"))
        );
    }

    #[test]
    fn recording_sink_still_works_alongside_renderer() {
        let events = vec![Event::StdoutChunk {
            node: "api".to_owned(),
            payload: OutputPayload::utf8("ok\n"),
        }];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut state = TaskOutputRendererState::default();
        let mut renderer = TaskOutputRenderer::from_state(
            TaskOutputMode::Live,
            &mut stdout,
            &mut stderr,
            &mut state,
        );
        let mut recorder = RecordingSink::new();
        for event in events {
            renderer.emit(event.clone());
            recorder.emit(event);
        }
        assert_eq!(recorder.events().len(), 1);
        assert_eq!(String::from_utf8(stdout).expect("utf-8"), "[api] ok\n");
    }
}
