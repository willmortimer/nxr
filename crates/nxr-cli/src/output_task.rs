//! Task execution output renderers implementing [`EventSink`].
//!
//! Wired from global `--output` and `--events`; execution plumbing is deferred to
//! parallel task scheduling (T1).

#![allow(dead_code)] // Public API wired in T1 parallel execution.

use std::collections::BTreeMap;
use std::io::{self, Write};

use clap::ValueEnum;
use nxr_task::{Event, EventSink, NullSink};

/// Multiplexed stdout/stderr presentation for parallel task runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum TaskOutputMode {
    /// Prefix each output line with `[node] ` as chunks arrive.
    Live,
    /// Buffer stdout/stderr per node; flush when the node exits.
    Grouped,
    /// Buffer per node; emit buffered output only on nonzero [`Event::NodeExited`].
    Failures,
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
    OutputOnly(TaskOutputRenderer<'a>),
    EventsOnly {
        format: EventsFormat,
        writer: &'a mut dyn Write,
    },
    Both {
        output: TaskOutputRenderer<'a>,
        format: EventsFormat,
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
        let inner = match (output, events) {
            (None, None) => TaskEventSinkInner::Null,
            (Some(mode), None) => {
                TaskEventSinkInner::OutputOnly(TaskOutputRenderer::new(mode, stdout, stderr))
            }
            (None, Some(format)) => TaskEventSinkInner::EventsOnly {
                format,
                writer: stderr,
            },
            (Some(mode), Some(format)) => TaskEventSinkInner::Both {
                output: TaskOutputRenderer::new(mode, stdout, stderr),
                format,
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
            TaskEventSinkInner::OutputOnly(renderer) => renderer.emit(event),
            TaskEventSinkInner::EventsOnly { format, writer } => {
                write_jsonl_event(*writer, *format, &event);
            }
            TaskEventSinkInner::Both { output, format } => {
                output.emit(event.clone());
                let mut stderr = io::stderr().lock();
                write_jsonl_event(&mut stderr, *format, &event);
            }
        }
    }
}

struct TaskOutputRenderer<'a> {
    mode: TaskOutputMode,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
    live_stdout_pending: BTreeMap<String, String>,
    live_stderr_pending: BTreeMap<String, String>,
    grouped: BTreeMap<String, NodeBuffers>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct NodeBuffers {
    stdout: String,
    stderr: String,
}

impl<'a> TaskOutputRenderer<'a> {
    fn new(mode: TaskOutputMode, stdout: &'a mut dyn Write, stderr: &'a mut dyn Write) -> Self {
        Self {
            mode,
            stdout,
            stderr,
            live_stdout_pending: BTreeMap::new(),
            live_stderr_pending: BTreeMap::new(),
            grouped: BTreeMap::new(),
        }
    }

    fn buffers_for(&mut self, node: &str) -> &mut NodeBuffers {
        self.grouped.entry(node.to_owned()).or_default()
    }
}

impl EventSink for TaskOutputRenderer<'_> {
    fn emit(&mut self, event: Event) {
        match event {
            Event::StdoutChunk { node, text } => match self.mode {
                TaskOutputMode::Live => {
                    let pending = self.live_stdout_pending.entry(node.clone()).or_default();
                    write_labeled_lines(self.stdout, &node, &text, pending);
                }
                TaskOutputMode::Grouped | TaskOutputMode::Failures => {
                    self.buffers_for(&node).stdout.push_str(&text);
                }
            },
            Event::StderrChunk { node, text } => match self.mode {
                TaskOutputMode::Live => {
                    let pending = self.live_stderr_pending.entry(node.clone()).or_default();
                    write_labeled_lines(self.stderr, &node, &text, pending);
                }
                TaskOutputMode::Grouped | TaskOutputMode::Failures => {
                    self.buffers_for(&node).stderr.push_str(&text);
                }
            },
            Event::NodeExited { node, code } => {
                if matches!(self.mode, TaskOutputMode::Live) {
                    flush_pending_line(self.stdout, &node, &mut self.live_stdout_pending);
                    flush_pending_line(self.stderr, &node, &mut self.live_stderr_pending);
                }

                let should_flush = match self.mode {
                    TaskOutputMode::Live => false,
                    TaskOutputMode::Grouped => true,
                    TaskOutputMode::Failures => node_failed(code),
                };

                if should_flush {
                    if let Some(buffers) = self.grouped.remove(&node) {
                        let _ = write_buffered_output(self.stdout, self.stderr, &buffers);
                    }
                } else if matches!(self.mode, TaskOutputMode::Failures) {
                    let _ = self.grouped.remove(&node);
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

struct JsonlEventsWriter<'a> {
    writer: &'a mut dyn Write,
}

impl<'a> JsonlEventsWriter<'a> {
    fn new(writer: &'a mut dyn Write) -> Self {
        Self { writer }
    }
}

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
        stdout.write_all(buffers.stdout.as_bytes())?;
        if !buffers.stdout.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
    }
    if !buffers.stderr.is_empty() {
        stderr.write_all(buffers.stderr.as_bytes())?;
        if !buffers.stderr.ends_with('\n') {
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

fn flush_pending_line(writer: &mut dyn Write, node: &str, pending: &mut BTreeMap<String, String>) {
    if let Some(rest) = pending.remove(node)
        && !rest.is_empty()
    {
        let prefix = format!("[{node}] ");
        let _ = writeln!(writer, "{prefix}{rest}");
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
                text: "listening\n".to_owned(),
            },
            Event::StdoutChunk {
                node: "web".to_owned(),
                text: "ready".to_owned(),
            },
            Event::StdoutChunk {
                node: "web".to_owned(),
                text: " in 421 ms\n".to_owned(),
            },
            Event::StderrChunk {
                node: "worker".to_owned(),
                text: "warn: retry\n".to_owned(),
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
        let mut sink = TaskOutputRenderer::new(mode, &mut stdout, &mut stderr);
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
                text: "partial".to_owned(),
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
                text: "hidden\n".to_owned(),
            },
            Event::StdoutChunk {
                node: "bad".to_owned(),
                text: "boom\n".to_owned(),
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
                text: "killed\n".to_owned(),
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
    fn composite_sink_with_no_options_is_inert() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut sink = build_task_event_sink(None, None, &mut stdout, &mut stderr);
        sink.emit(Event::StdoutChunk {
            node: "api".to_owned(),
            text: "ignored\n".to_owned(),
        });
        drop(sink);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn recording_sink_still_works_alongside_renderer() {
        let events = vec![Event::StdoutChunk {
            node: "api".to_owned(),
            text: "ok\n".to_owned(),
        }];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut renderer = TaskOutputRenderer::new(TaskOutputMode::Live, &mut stdout, &mut stderr);
        let mut recorder = RecordingSink::new();
        for event in events {
            renderer.emit(event.clone());
            recorder.emit(event);
        }
        assert_eq!(recorder.events().len(), 1);
        assert_eq!(String::from_utf8(stdout).expect("utf-8"), "[api] ok\n");
    }
}
