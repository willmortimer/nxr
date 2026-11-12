//! Local stdio MCP server for the operation catalog.
//!
//! The workspace is the checkout this process was launched in. A tool argument
//! cannot select another checkout. Execution stays behind `execution_allowed`.

use std::io::{self, BufRead, Write};

use nxr_nix::OptionalNixFlags;
use nxr_task::{OperationRecord, admit, cancel, execution_fingerprint, inspect, mark_started};
use serde_json::{Value, json};

use crate::commands::op::load_records;
use crate::runner_output::RunnerOutput;

/// Serve one MCP session on stdin and stdout.
///
/// # Errors
///
/// Returns an error when the catalog cannot be loaded or stdio fails.
pub fn serve(
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<i32, String> {
    let records = load_records(flake, nix_override, nix_flags, runner)?;
    let workspace_id = records
        .first()
        .map(|record| record.workspace_id.clone())
        .unwrap_or_else(|| "workspace".to_owned());
    let run_dir = std::env::temp_dir().join(format!("nxr-mcp-{workspace_id}"));
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout().lock();
    loop {
        let Some(body) = read_frame(&mut reader)? else {
            return Ok(0);
        };
        let message: Value = serde_json::from_str(&body).map_err(|err| err.to_string())?;
        if message.get("id").is_none() {
            continue;
        }
        let response = respond(&message, &records, &workspace_id, &run_dir);
        write_frame(&mut stdout, &response)?;
    }
}

fn respond(
    message: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    run_dir: &std::path::Path,
) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "nxr", "version": "0.1.0" }
        }),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => match call_tool(&params, records, workspace_id, run_dir) {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "isError": false
            }),
            Err(error) => json!({
                "content": [{ "type": "text", "text": error }],
                "isError": true
            }),
        },
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" }
            });
        }
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn tools() -> Vec<Value> {
    [
        "operations_search",
        "operations_describe",
        "operations_plan",
        "runs_inspect",
        "workspace_affected",
        "operations_start",
        "runs_get_logs",
        "runs_cancel",
    ]
    .into_iter()
    .map(|name| json!({ "name": name, "inputSchema": { "type": "object" } }))
    .collect()
}

/// Dispatch one tool call. Workspace path arguments are rejected.
///
/// # Errors
///
/// Returns an error when the tool is unknown, the operation cannot run, or the key conflicts.
pub fn call_tool(
    params: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    run_dir: &std::path::Path,
) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    if arguments.get("workspace").is_some()
        || arguments.get("workspace_root").is_some()
        || arguments.get("checkout").is_some()
    {
        return Err("the workspace is bound at launch".to_owned());
    }
    match name {
        "operations_search" => {
            let query = arguments.get("query").and_then(Value::as_str).unwrap_or("");
            let matched = nxr_task::search(records, query);
            Ok(json!(matched))
        }
        "operations_describe" => {
            let id = required_id(&arguments)?;
            nxr_task::describe(records, id)
                .map(|record| json!(record))
                .ok_or_else(|| format!("unknown operation {id}"))
        }
        "operations_plan" => {
            let id = required_id(&arguments)?;
            let record =
                nxr_task::describe(records, id).ok_or_else(|| format!("unknown operation {id}"))?;
            Ok(json!(nxr_task::plan(record)))
        }
        "workspace_affected" => Ok(json!(
            records
                .iter()
                .map(|record| &record.operation_id)
                .collect::<Vec<_>>()
        )),
        "runs_inspect" => {
            let run_id = arguments
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(json!(inspect(run_dir, run_id, workspace_id)?))
        }
        "runs_get_logs" => {
            let run_id = arguments
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let found = inspect(run_dir, run_id, workspace_id)?;
            Ok(
                json!({ "run_id": run_id, "status": found.as_ref().map(|row| row.status.clone()), "log": "" }),
            )
        }
        "operations_start" => start(&arguments, records, workspace_id, run_dir),
        "runs_cancel" => {
            let run_id = arguments
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(json!({ "cancelled": cancel(run_dir, run_id, workspace_id)? }))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn start(
    arguments: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    run_dir: &std::path::Path,
) -> Result<Value, String> {
    let id = required_id(arguments)?;
    let key = arguments
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if key.is_empty() {
        return Err("idempotency_key is required".to_owned());
    }
    let record =
        nxr_task::describe(records, id).ok_or_else(|| format!("unknown operation {id}"))?;
    if record.workspace_id != workspace_id {
        return Err("operation is not in the bound workspace".to_owned());
    }
    let fingerprint = execution_fingerprint(record, &[]);
    match admit(run_dir, key, record, &fingerprint)? {
        nxr_task::Admission::Reserved(reservation) => {
            mark_started(run_dir, &reservation.run_id, workspace_id)?;
            Ok(json!({ "run_id": reservation.run_id, "started": true, "replayed": false }))
        }
        nxr_task::Admission::Existing(reservation) => {
            Ok(json!({ "run_id": reservation.run_id, "started": false, "replayed": true }))
        }
        nxr_task::Admission::Conflict(reservation) => Err(format!(
            "idempotency key conflicts with {}",
            reservation.run_id
        )),
        nxr_task::Admission::Refused => Err("operation is not executable".to_owned()),
    }
}

fn required_id(arguments: &Value) -> Result<&str, String> {
    arguments
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "id is required".to_owned())
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|err| err.to_string())?,
            );
        }
    }
    let Some(length) = length else {
        return Err("missing Content-Length".to_owned());
    };
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|err| err.to_string())?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|err| err.to_string())
}

fn write_frame(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|err| err.to_string())?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).map_err(|err| err.to_string())?;
    writer.write_all(&body).map_err(|err| err.to_string())?;
    writer.flush().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::{Declaration, OperationKind, WorkspaceInstance, operation_record};

    fn records() -> Vec<OperationRecord> {
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: "/tmp/lattice".to_owned(),
            source_revision: Some("abc".to_owned()),
        };
        vec![
            operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "test", "flake.nix"),
            ),
            operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "reset-db", "flake.nix"),
            ),
        ]
    }

    #[test]
    fn describe_matches_the_catalog_record_and_start_is_allowlisted() {
        let records = records();
        let described = call_tool(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:test"}}),
            &records,
            "lattice",
            std::path::Path::new("/tmp/unused"),
        )
        .unwrap();
        assert_eq!(described["operation_id"], "lattice:task:test");
        assert_eq!(described["execution_allowed"], true);
        let dir = tempfile::tempdir().unwrap();
        let started = call_tool(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "once"}}),
            &records,
            "lattice",
            dir.path(),
        )
        .unwrap();
        let again = call_tool(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "once"}}),
            &records,
            "lattice",
            dir.path(),
        )
        .unwrap();
        assert_eq!(started["run_id"], again["run_id"]);
        assert_eq!(again["replayed"], true);
        let refused = call_tool(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:reset-db", "idempotency_key": "nope"}}),
            &records,
            "lattice",
            dir.path(),
        );
        assert!(refused.is_err());
        let foreign = call_tool(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:test", "workspace_root": "/other"}}),
            &records,
            "lattice",
            dir.path(),
        );
        assert!(foreign.is_err());
    }
}
