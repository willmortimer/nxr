//! Task execution output renderers implementing [`EventSink`].
//!
//! Wired from global `--output` and `--events` for parallel and labeled runs.
//! Human modes decode chunk bytes with an incremental UTF-8 decoder so 4 KiB
//! pipe reads never split multi-byte characters into replacement garbage.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Seek, SeekFrom, Write};

use clap::ValueEnum;
use nxr_task::{Event, EventSink, NullSink, OutputPayload};
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
    /// Single foreground child inherits stdio (no pipe multiplexing).
    ///
    /// Conflicts with `-j > 1` and `--events`; handled before the event sink.
    Raw,
}

impl TaskOutputMode {
    /// Modes that require piped child stdio and a renderer.
    #[must_use]
    pub const fn is_multiplexed(self) -> bool {
        matches!(self, Self::Live | Self::Grouped | Self::Failures)
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
                let mut renderer =
                    TaskOutputRenderer::from_state(*mode, stdout, stderr, state);
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
                    let mut renderer =
                        TaskOutputRenderer::from_state(*mode, stdout, stderr, state);
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
    stdout_decoder: Utf8StreamDecoder,
    stderr_decoder: Utf8StreamDecoder,
    stdout: SpillableBuffer,
    stderr: SpillableBuffer,
}

/// Buffered stream data held in memory until [`BUFFER_SPILL_THRESHOLD`], then
/// appended to a temp file so grouped/failures modes stay bounded.
#[derive(Debug, Default)]
struct SpillableBuffer {
    memory: String,
    spill: Option<NamedTempFile>,
    ends_with_newline: bool,
}

impl SpillableBuffer {
    fn push_str(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }

        if self.spill.is_some() {
            if let Some(file) = self.spill.as_mut() {
                let _ = file.write_all(chunk.as_bytes());
                let _ = file.flush();
            }
            self.ends_with_newline = chunk.as_bytes().last() == Some(&b'\n');
            return;
        }

        if self.memory.len().saturating_add(chunk.len()) > BUFFER_SPILL_THRESHOLD {
            if let Ok(mut file) = NamedTempFile::new() {
                let _ = file.write_all(self.memory.as_bytes());
                let _ = file.write_all(chunk.as_bytes());
                let _ = file.flush();
                self.memory.clear();
                self.spill = Some(file);
                self.ends_with_newline = chunk.as_bytes().last() == Some(&b'\n');
            } else {
                self.memory.push_str(chunk);
                self.ends_with_newline = self.memory.ends_with('\n');
            }
            return;
        }

        self.memory.push_str(chunk);
        self.ends_with_newline = self.memory.ends_with('\n');
    }

    fn write_to(&self, writer: &mut dyn Write) -> io::Result<()> {
        if let Some(file) = &self.spill {
            let mut reader = BufReader::new(file.as_file());
            reader.seek(SeekFrom::Start(0))?;
            io::copy(&mut reader, writer)?;
        }
        if !self.memory.is_empty() {
            writer.write_all(self.memory.as_bytes())?;
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

    fn ingest_live(&mut self, is_stdout: bool, node: &str, payload: &OutputPayload) {
        let map = if is_stdout {
            &mut self.state.live_stdout
        } else {
            &mut self.state.live_stderr
        };
        let state = map.entry(node.to_owned()).or_default();
        let decoded = state.decoder.push(payload.as_bytes());
        let writer = if is_stdout {
            &mut *self.stdout
        } else {
            &mut *self.stderr
        };
        write_labeled_lines(writer, node, &decoded, &mut state.pending);
    }

    fn ingest_buffered(&mut self, is_stdout: bool, node: &str, payload: &OutputPayload) {
        let entry = self.state.grouped.entry(node.to_owned()).or_default();
        if is_stdout {
            let decoded = entry.stdout_decoder.push(payload.as_bytes());
            entry.stdout.push_str(&decoded);
        } else {
            let decoded = entry.stderr_decoder.push(payload.as_bytes());
            entry.stderr.push_str(&decoded);
        }
    }

    fn flush_live_partial(&mut self, node: &str) {
        flush_stream_on_exit(self.stdout, node, &mut self.state.live_stdout);
        flush_stream_on_exit(self.stderr, node, &mut self.state.live_stderr);
    }

    fn finish_buffered_decoders(&mut self, node: &str) {
        if let Some(buffers) = self.state.grouped.get_mut(node) {
            let rest = buffers.stdout_decoder.finish();
            buffers.stdout.push_str(&rest);
            let rest = buffers.stderr_decoder.finish();
            buffers.stderr.push_str(&rest);
        }
    }
}

fn flush_stream_on_exit(
    writer: &mut dyn Write,
    node: &str,
    streams: &mut BTreeMap<String, StreamState>,
) {
    if let Some(mut state) = streams.remove(node) {
        let rest = state.decoder.finish();
        if !rest.is_empty() {
            state.pending.push_str(&rest);
        }
        if !state.pending.is_empty() {
            let prefix = format!("[{node}] ");
            let _ = writeln!(writer, "{prefix}{}", state.pending);
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
                TaskOutputMode::Raw => {}
            },
            Event::StderrChunk { node, payload } => match self.mode {
                TaskOutputMode::Live => self.ingest_live(false, &node, &payload),
                TaskOutputMode::Grouped | TaskOutputMode::Failures => {
                    self.ingest_buffered(false, &node, &payload);
                }
                TaskOutputMode::Raw => {}
            },
            Event::NodeExited { node, code } => {
                if matches!(self.mode, TaskOutputMode::Live) {
                    self.flush_live_partial(&node);
                }

                if matches!(
                    self.mode,
                    TaskOutputMode::Grouped | TaskOutputMode::Failures
                ) {
                    self.finish_buffered_decoders(&node);
                }

                let should_flush = match self.mode {
                    TaskOutputMode::Live | TaskOutputMode::Raw => false,
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
            }
            Event::Diagnostic { message } => {
                let _ = writeln!(self.stderr, "{message}");
            }
            Event::PlanCreated { .. }
            | Event::NodeQueued { .. }
            | Event::NodeStarted { .. }
            | Event::RunCompleted { .. } => {}
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

fn write_labeled_lines(writer: &mut dyn Write, node: &str, text: &str, pending: &mut String) {
    pending.push_str(text);
    let prefix = format!("[{node}] ");

    while let Some(newline_idx) = pending.find('\n') {
        let line = pending.drain(..=newline_idx).collect::<String>();
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let _ = writeln!(writer, "{prefix}{line}");
    }
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
            Event::NodeExited {
                node: "api".to_owned(),
                code: Some(0),
            },
            Event::NodeExited {
                node: "web".to_owned(),
                code: Some(0),
            },
            Event::NodeExited {
                node: "worker".to_owned(),
                code: Some(1),
            },
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
            Event::NodeExited {
                node: "api".to_owned(),
                code: Some(0),
            },
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
            Event::NodeExited {
                node: "api".to_owned(),
                code: Some(0),
            },
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
            Event::NodeExited {
                node: "t".to_owned(),
                code: Some(0),
            },
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
            Event::NodeExited {
                node: "t".to_owned(),
                code: Some(0),
            },
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
            Event::NodeExited {
                node: "ok".to_owned(),
                code: Some(0),
            },
            Event::NodeExited {
                node: "bad".to_owned(),
                code: Some(2),
            },
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
            Event::NodeExited {
                node: "sig".to_owned(),
                code: None,
            },
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
        events.push(Event::NodeExited {
            node: "n".to_owned(),
            code: Some(0),
        });
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
            &[
                event,
                Event::NodeExited {
                    node: "bin".to_owned(),
                    code: Some(0),
                },
            ],
        );
        assert!(stdout.starts_with("[bin] "));
        assert!(stdout.contains('A'));
    }

    #[test]
    fn jsonl_events_writer_emits_one_line_per_event() {
        let events = vec![
            Event::NodeStarted {
                node: "fmt".to_owned(),
            },
            Event::RunCompleted { success: true },
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
        sink.emit(Event::NodeStarted {
            node: "fmt".to_owned(),
        });
        sink.emit(Event::StdoutChunk {
            node: "fmt".to_owned(),
            payload: OutputPayload::utf8("ok\n"),
        });
        sink.emit(Event::NodeExited {
            node: "fmt".to_owned(),
            code: Some(0),
        });
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
            Event::NodeExited {
                node: "big".to_owned(),
                code: Some(0),
            },
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
