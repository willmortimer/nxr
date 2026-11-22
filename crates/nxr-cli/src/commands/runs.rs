//! Bounded, read-only access to the same foreground evidence used by renderers.

use std::io::Write;
use std::path::Path;

use clap::{Subcommand, ValueEnum};
use nxr_task::run_journal::LogStream;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn journal(self) -> LogStream {
        match self {
            Self::Stdout => LogStream::Stdout,
            Self::Stderr => LogStream::Stderr,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum RunsCommand {
    /// Inspect the replayed execution projection
    Inspect { run_id: String },
    /// Show bounded normalized diagnostics
    Diagnostics {
        run_id: String,
        #[arg(long)]
        node: Option<String>,
    },
    /// Retrieve a byte-exact log slice
    Logs {
        run_id: String,
        #[arg(long)]
        node: String,
        #[arg(long, value_enum, default_value = "stderr")]
        stream: Stream,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 4096)]
        limit: usize,
        /// Return the last N lines within a bounded 64 KiB tail
        #[arg(long, conflicts_with = "offset")]
        tail: Option<usize>,
    },
    /// Retrieve declarations and independently verified artifact references
    Artifacts {
        run_id: String,
        #[arg(long)]
        node: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 64)]
        limit: u64,
    },
}

pub fn has_dag(run_id: &str) -> bool {
    super::run_store::directory(run_id).is_ok_and(|path| path.join("events.sqlite").is_file())
}

/// The workspace binding is mandatory for MCP and optional for the user's CLI.
pub fn query(name: &str, arguments: &Value, workspace: Option<&Path>) -> Result<Value, String> {
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .ok_or("run_id is required")?;
    let reader = super::run_store::reader(run_id, workspace)?;
    let node = arguments
        .get("node")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "node must be a string".to_owned())
        })
        .transpose()?;
    match name {
        "runs_inspect" => serde_json::to_value(reader.view()?).map_err(|error| error.to_string()),
        "runs_get_summary" => {
            let view = reader.view()?;
            Ok(
                json!({"schema_version":view.schema_version,"run_id":run_id,"state":view.state,"last_sequence":view.last_sequence,"summary":view.summary,"terminal_receipt_observed":view.terminal_receipt_observed}),
            )
        }
        "runs_get_diagnostics" => {
            let (diagnostics, truncated) = reader.diagnostics(node)?;
            let diagnostics: Vec<_> = diagnostics
                .into_iter()
                .map(|item| {
                    let mut value = serde_json::to_value(&item).unwrap_or(Value::Null);
                    value["category"] = json!(item.code);
                    value["excerpt"] = json!(item.message);
                    value["file"] = json!(item.location.as_ref().map(|location| &location.file));
                    value["line"] =
                        json!(item.location.as_ref().and_then(|location| location.line));
                    value
                })
                .collect();
            Ok(json!({"run_id":run_id,"diagnostics":diagnostics,"truncated":truncated}))
        }
        "runs_get_artifacts" => {
            let after = unsigned_argument(arguments, "after", 0)?;
            let limit = unsigned_argument(arguments, "limit", 64)?.clamp(1, 256);
            let records = reader.artifacts(node, after, limit)?;
            let next = records.last().map_or(after, |record| record.sequence);
            Ok(
                json!({"run_id":run_id,"records":records,"next_cursor":next,"page_full":records.len() as u64 == limit}),
            )
        }
        "runs_get_logs" => {
            let node = node.ok_or("node is required for a foreground DAG log")?;
            let stream = match arguments
                .get("stream")
                .and_then(Value::as_str)
                .unwrap_or("stderr")
            {
                "stdout" => LogStream::Stdout,
                "stderr" => LogStream::Stderr,
                _ => return Err("stream must be stdout or stderr".to_owned()),
            };
            let offset = unsigned_argument(arguments, "offset", 0)?;
            let limit = unsigned_argument(arguments, "limit", 4096)?.min(65536) as usize;
            let slice = reader.read_node_log(node, stream, offset, limit)?;
            let payload = serde_json::to_value(nxr_task::OutputPayload::from_bytes(slice.bytes))
                .map_err(|error| error.to_string())?;
            Ok(
                json!({"run_id":run_id,"node":node,"stream":stream,"offset":offset,"next_offset":slice.next_offset,"eof":slice.eof,"text":payload["text"],"encoding":payload.get("encoding").and_then(Value::as_str).unwrap_or("utf8"),"payload":payload}),
            )
        }
        _ => Err("unsupported run query".to_owned()),
    }
}

fn unsigned_argument(arguments: &Value, name: &str, default: u64) -> Result<u64, String> {
    arguments
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("{name} must be a nonnegative integer"))
        })
        .unwrap_or(Ok(default))
}

pub fn run(command: &RunsCommand, json_output: bool) -> Result<i32, String> {
    if let RunsCommand::Logs {
        run_id,
        node,
        stream,
        offset,
        limit,
        tail,
    } = command
        && !json_output
    {
        let reader = super::run_store::reader(run_id, None)?;
        let slice = if let Some(lines) = tail {
            // Find the stream end without reading its content, then retrieve one bounded tail.
            let end = reader.log_length(node, stream.journal())?;
            let mut slice =
                reader.read_node_log(node, stream.journal(), end.saturating_sub(65536), 65536)?;
            let start = tail_start(&slice.bytes, *lines);
            slice.bytes.drain(..start);
            slice
        } else {
            reader.read_node_log(node, stream.journal(), *offset, *limit)?
        };
        std::io::stdout()
            .lock()
            .write_all(&slice.bytes)
            .map_err(|error| error.to_string())?;
        return Ok(0);
    }
    let (name, arguments) = match command {
        RunsCommand::Inspect { run_id } => ("runs_inspect", json!({"run_id":run_id})),
        RunsCommand::Diagnostics { run_id, node } => {
            ("runs_get_diagnostics", json!({"run_id":run_id,"node":node}))
        }
        RunsCommand::Artifacts {
            run_id,
            node,
            after,
            limit,
        } => (
            "runs_get_artifacts",
            json!({"run_id":run_id,"node":node,"after":after,"limit":limit}),
        ),
        RunsCommand::Logs {
            run_id,
            node,
            stream,
            offset,
            limit,
            tail,
        } => {
            if tail.is_some() {
                return Err(
                    "--tail is available for byte output; JSON callers use offsets".to_owned(),
                );
            }
            (
                "runs_get_logs",
                json!({"run_id":run_id,"node":node,"stream":stream.journal().as_str(),"offset":offset,"limit":limit}),
            )
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&query(name, &arguments, None)?)
            .map_err(|error| error.to_string())?
    );
    Ok(0)
}

fn tail_start(bytes: &[u8], lines: usize) -> usize {
    if lines == 0 {
        return bytes.len();
    }
    let end = bytes
        .len()
        .saturating_sub(usize::from(bytes.last() == Some(&b'\n')));
    bytes[..end]
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(lines - 1)
        .map_or(0, |(index, _)| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tail_has_exact_line_count_with_and_without_final_newline() {
        for bytes in [b"a\nb\nc\n".as_slice(), b"a\nb\nc".as_slice()] {
            assert_eq!(
                &bytes[tail_start(bytes, 1)..],
                if bytes.last() == Some(&b'\n') {
                    b"c\n".as_slice()
                } else {
                    b"c".as_slice()
                }
            );
            assert_eq!(tail_start(bytes, 0), bytes.len());
            assert_eq!(tail_start(bytes, 20), 0);
        }
        assert_eq!(tail_start(b"", 1), 0);
    }
}
