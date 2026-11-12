//! Local stdio MCP server for the operation catalog.
//!
//! The workspace is the checkout this process was launched in. A tool argument
//! cannot select another checkout. Execution stays behind `execution_allowed`.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use nxr_nix::OptionalNixFlags;
use nxr_task::{
    OperationRecord, admit, bind_supervisor, cancel, execution_fingerprint, inspect,
    mark_admitting, mark_reconciliation,
};
use serde_json::{Value, json};

use crate::commands::affected::{AffectedPathSources, select_for_flake};
use crate::commands::op::load_records;
use crate::output_options::{ColorWhen, LogFormat, OutputOptions};
use crate::runner_output::RunnerOutput;

fn quiet_runner() -> RunnerOutput {
    RunnerOutput::new(OutputOptions::new(
        1,
        0,
        true,
        true,
        ColorWhen::Never,
        LogFormat::Human,
    ))
}

/// How `operations_start` launches work. Production uses `nxr task`.
#[derive(Clone, Debug)]
pub enum Launch {
    /// `nxr task <name>` in the bound workspace.
    Task,
    /// A direct argv, used to prove logs and cancellation without a full suite.
    Command(Vec<String>),
}

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
    let workspace_root = records
        .first()
        .map(|record| PathBuf::from(&record.workspace_root))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let run_dir = reservation_dir(&workspace_id, &workspace_root);
    let supervisor_dir = run_dir.join("supervisor");
    let trusted = workspace_trusted(&workspace_root);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout().lock();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(0);
        }
        let Some(response) = handle_line(
            line.trim(),
            &records,
            &workspace_id,
            &workspace_root,
            &run_dir,
            &supervisor_dir,
            trusted,
            flake,
            nix_override,
            nix_flags,
            runner,
            &Launch::Task,
            true,
        ) else {
            continue;
        };
        writeln!(stdout, "{response}").map_err(|err| err.to_string())?;
        stdout.flush().map_err(|err| err.to_string())?;
    }
}

/// One JSON-RPC object per line. A notification returns nothing. A bad line returns an error.
#[allow(clippy::too_many_arguments)]
pub fn handle_line(
    line: &str,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    supervisor_dir: &Path,
    trusted: bool,
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
    launch: &Launch,
    reload: bool,
) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => {
            return Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": "parse error" }
                })
                .to_string(),
            );
        }
    };
    if message.get("id").is_none() {
        return None;
    }
    let response = respond(
        &message,
        records,
        workspace_id,
        workspace_root,
        run_dir,
        supervisor_dir,
        trusted,
        flake,
        nix_override,
        nix_flags,
        runner,
        launch,
        reload,
    );
    Some(response.to_string())
}

#[allow(clippy::too_many_arguments)]
fn respond(
    message: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    supervisor_dir: &Path,
    trusted: bool,
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
    launch: &Launch,
    reload: bool,
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
        "tools/call" => match call_tool(
            &params,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            supervisor_dir,
            trusted,
            flake,
            nix_override,
            nix_flags,
            runner,
            launch,
            reload,
        ) {
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
    vec![
        tool(
            "operations_search",
            "Find operations in the bound workspace",
            &["query"],
            json!({"query": {"type": "string", "description": "Text matched against name, alias, or id"}}),
        ),
        tool(
            "operations_describe",
            "Show one operation record",
            &["id"],
            json!({"id": {"type": "string", "description": "Portable operation id"}}),
        ),
        tool(
            "operations_plan",
            "Show the plan for one operation",
            &["id"],
            json!({"id": {"type": "string", "description": "Portable operation id"}}),
        ),
        tool(
            "runs_inspect",
            "Read one run in this workspace",
            &["run_id"],
            json!({"run_id": {"type": "string", "description": "Supervisor run id"}}),
        ),
        tool(
            "workspace_affected",
            "List operations affected by paths in this workspace",
            &[],
            json!({"paths": {"type": "array", "items": {"type": "string"}, "description": "Repository-relative paths"}}),
        ),
        tool(
            "operations_start",
            "Start an allowlisted local operation",
            &["id", "idempotency_key"],
            json!({
                "id": {"type": "string", "description": "Portable operation id"},
                "idempotency_key": {"type": "string", "description": "Caller key for this attempt"}
            }),
        ),
        tool(
            "runs_get_logs",
            "Read a bounded slice of a run log",
            &["run_id"],
            json!({
                "run_id": {"type": "string"},
                "offset": {"type": "integer", "description": "Byte offset"},
                "limit": {"type": "integer", "description": "Maximum bytes, at most 65536"}
            }),
        ),
        tool(
            "runs_cancel",
            "Cancel a run owned by this workspace",
            &["run_id"],
            json!({"run_id": {"type": "string"}}),
        ),
    ]
}

fn tool(name: &str, description: &str, required: &[&str], properties: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        }
    })
}

/// Dispatch one tool call. Workspace path arguments are rejected.
///
/// # Errors
///
/// Returns an error when the tool is unknown, the operation cannot run, or the key conflicts.
#[allow(clippy::too_many_arguments)]
pub fn call_tool(
    params: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    supervisor_dir: &Path,
    trusted: bool,
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
    launch: &Launch,
    reload: bool,
) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    if arguments.get("workspace").is_some()
        || arguments.get("workspace_root").is_some()
        || arguments.get("checkout").is_some()
    {
        return Err("the workspace is bound at launch".to_owned());
    }
    if let Some(allowed) = known_arguments(name)
        && arguments
            .as_object()
            .is_some_and(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return Err("unknown argument".to_owned());
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
        "workspace_affected" => affected_ids(
            flake,
            nix_override,
            nix_flags,
            runner,
            &arguments,
            workspace_id,
        ),
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
            let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(4096)
                .min(65536) as usize;
            let (log, next) = read_log(supervisor_dir, run_id, offset, limit);
            let durable = nxr_core::load_durable_run(run_id).ok();
            Ok(json!({
                "run_id": run_id,
                "status": durable.as_ref().map(|row| format!("{:?}", row.status)).or_else(|| found.as_ref().map(|row| row.status.clone())),
                "exit_code": durable.as_ref().and_then(|row| row.exit_code),
                "log": log,
                "next_offset": next
            }))
        }
        "operations_start" => start(
            &arguments,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            supervisor_dir,
            trusted,
            flake,
            nix_override,
            nix_flags,
            runner,
            launch,
            reload,
        ),
        "runs_cancel" => {
            let run_id = arguments
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let owned = inspect(run_dir, run_id, workspace_id)?;
            if owned.is_none() {
                return Ok(json!({ "cancelled": false }));
            }
            let stopped = nxr_core::execution_owner::cancel(supervisor_dir, run_id, false).is_ok();
            let marked = cancel(run_dir, run_id, workspace_id)?;
            Ok(json!({ "cancelled": stopped || marked }))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn start(
    arguments: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    supervisor_dir: &Path,
    trusted: bool,
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
    launch: &Launch,
    reload: bool,
) -> Result<Value, String> {
    if !trusted {
        return Err("workspace is not trusted".to_owned());
    }
    let id = required_id(arguments)?;
    let key = arguments
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if key.is_empty() {
        return Err("idempotency_key is required".to_owned());
    }
    let fresh = if reload {
        load_records(flake, nix_override, nix_flags, runner)?
    } else {
        records.to_vec()
    };
    let record = nxr_task::describe(&fresh, id).ok_or_else(|| format!("unknown operation {id}"))?;
    if record.workspace_id != workspace_id {
        return Err("operation is not in the bound workspace".to_owned());
    }
    let contents = read_inputs(workspace_root, &record.inputs)?;
    let borrowed = contents
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect::<Vec<_>>();
    let fingerprint = execution_fingerprint(record, &borrowed);
    match admit(run_dir, key, record, &fingerprint)? {
        nxr_task::Admission::Reserved(_) => {
            mark_admitting(run_dir, key)?;
            let command = match launch {
                Launch::Task => vec!["nxr".to_owned(), "task".to_owned(), record.name.clone()],
                Launch::Command(argv) => argv.clone(),
            };
            match nxr_core::execution_owner::start(
                supervisor_dir,
                &json!({"command": command, "label": record.operation_id}),
            ) {
                Ok(value) => {
                    let run_id = value
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "supervisor did not return a run id".to_owned())?
                        .to_owned();
                    let durable =
                        nxr_core::load_durable_run(&run_id).map_err(|err| err.to_string())?;
                    if durable.status != nxr_core::DurableRunStatus::Running
                        && durable.status != nxr_core::DurableRunStatus::Succeeded
                        && durable.status != nxr_core::DurableRunStatus::Failed
                    {
                        let _ = mark_reconciliation(run_dir, key);
                        return Err("supervisor did not admit the run".to_owned());
                    }
                    bind_supervisor(run_dir, key, &run_id)?;
                    Ok(json!({
                        "run_id": run_id,
                        "started": true,
                        "replayed": false,
                        "status": format!("{:?}", durable.status)
                    }))
                }
                Err(error) => {
                    let _ = mark_reconciliation(run_dir, key);
                    Err(error)
                }
            }
        }
        nxr_task::Admission::Existing(reservation) => Ok(json!({
            "run_id": reservation.run_id,
            "started": false,
            "replayed": true,
            "status": reservation.status
        })),
        nxr_task::Admission::Conflict(reservation) => Err(format!(
            "idempotency key conflicts with {}",
            reservation.run_id
        )),
        nxr_task::Admission::Refused => Err("operation is not executable".to_owned()),
    }
}

fn read_inputs<'a>(root: &Path, inputs: &'a [String]) -> Result<Vec<(&'a str, Vec<u8>)>, String> {
    let mut contents = Vec::new();
    for input in inputs {
        if input.starts_with('/') || input.split('/').any(|part| part == "..") {
            return Err(format!("input path is not inside the workspace: {input}"));
        }
        let bytes = std::fs::read(root.join(input))
            .map_err(|_| format!("declared input cannot be read: {input}"))?;
        contents.push((input.as_str(), bytes));
    }
    Ok(contents)
}

fn required_id(arguments: &Value) -> Result<&str, String> {
    arguments
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "id is required".to_owned())
}

fn known_arguments(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "operations_search" => &["query"],
        "operations_describe" | "operations_plan" => &["id"],
        "runs_inspect" | "runs_cancel" => &["run_id"],
        "workspace_affected" => &["paths"],
        "operations_start" => &["id", "idempotency_key"],
        "runs_get_logs" => &["run_id", "offset", "limit"],
        _ => return None,
    })
}

fn affected_ids(
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
    arguments: &Value,
    workspace_id: &str,
) -> Result<Value, String> {
    let paths = arguments
        .get("paths")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selection = select_for_flake(
        flake,
        nix_override,
        false,
        nix_flags,
        &AffectedPathSources {
            base: None,
            working_tree: paths.is_empty(),
            all_changes: None,
        },
        false,
        &paths,
        runner,
    )
    .map_err(|err| err.to_string())?;
    let ids = selection
        .analysis
        .tasks
        .iter()
        .map(|name| format!("{workspace_id}:task:{name}"))
        .collect::<Vec<_>>();
    Ok(json!(ids))
}

fn read_log(dir: &Path, run_id: &str, offset: u64, limit: usize) -> (String, u64) {
    let path = dir.join(format!("{run_id}.log"));
    let bytes = std::fs::read(path).unwrap_or_default();
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start.saturating_add(limit).min(bytes.len());
    (
        String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        end as u64,
    )
}

fn reservation_dir(workspace_id: &str, workspace_root: &Path) -> PathBuf {
    let material = format!("{workspace_id}\n{}", workspace_root.display());
    let digest = nxr_core::digest_bytes(material.as_bytes());
    nxr_core::state_dir()
        .join("mcp-reservations")
        .join(&digest[..32])
}

fn workspace_trusted(root: &Path) -> bool {
    if nxr_core::trust_project_env_enabled() {
        return true;
    }
    let Ok(key) = nxr_core::canonical_project_key(root) else {
        return false;
    };
    nxr_core::TrustDatabase::load()
        .ok()
        .is_some_and(|database| database.is_trusted(&key))
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

    fn forget_run(run_id: &str) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let dir = nxr_core::state_dir().join("runs");
        for suffix in [".json", ".events.jsonl", ".lock"] {
            let _ = std::fs::remove_file(dir.join(format!("{run_id}{suffix}")));
        }
    }

    fn invoke(
        params: &Value,
        records: &[OperationRecord],
        root: &std::path::Path,
        dir: &std::path::Path,
        trusted: bool,
        launch: &Launch,
    ) -> Result<Value, String> {
        call_tool(
            params,
            records,
            "lattice",
            root,
            dir,
            &dir.join("supervisor"),
            trusted,
            None,
            None,
            &OptionalNixFlags::default(),
            quiet_runner(),
            launch,
            false,
        )
    }

    #[test]
    fn describe_matches_the_catalog_record_and_start_is_allowlisted() {
        let records = records();
        let root = std::path::Path::new("/tmp/lattice");
        let described = invoke(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:test"}}),
            &records,
            root,
            std::path::Path::new("/tmp/unused"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(described["operation_id"], "lattice:task:test");
        assert_eq!(described["execution_allowed"], true);
        let dir = tempfile::tempdir().unwrap();
        let launch = Launch::Command(vec!["/bin/echo".into(), "ok".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "once"}}),
            &records,
            root,
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(started["started"], true);
        let again = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "once"}}),
            &records,
            root,
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(started["run_id"], again["run_id"]);
        assert_eq!(again["replayed"], true);
        assert_eq!(again["started"], false);
        let refused = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:reset-db", "idempotency_key": "nope"}}),
            &records,
            root,
            dir.path(),
            true,
            &launch,
        );
        assert!(refused.is_err());
        let foreign = invoke(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:test", "workspace_root": "/other"}}),
            &records,
            root,
            dir.path(),
            true,
            &launch,
        );
        assert!(foreign.is_err());
        let untrusted = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "other"}}),
            &records,
            root,
            dir.path(),
            false,
            &launch,
        );
        assert!(untrusted.unwrap_err().contains("not trusted"));
        forget_run(started["run_id"].as_str().unwrap());
    }

    #[test]
    fn newline_session_answers_requests_and_skips_notifications() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"operations_search","arguments":{"query":"test"}}}"#,
            "not-json",
        ];
        let mut responses = Vec::new();
        for line in lines {
            if let Some(response) = handle_line(
                line,
                &records,
                "lattice",
                std::path::Path::new("/tmp/lattice"),
                dir.path(),
                &dir.path().join("supervisor"),
                true,
                None,
                None,
                &OptionalNixFlags::default(),
                quiet_runner(),
                &Launch::Task,
                false,
            ) {
                responses.push(response);
            }
        }
        assert_eq!(responses.len(), 4);
        let listed: Value = serde_json::from_str(&responses[1]).unwrap();
        let schema = &listed["result"]["tools"][5]["inputSchema"];
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("idempotency_key"))
        );
        let searched: Value = serde_json::from_str(&responses[2]).unwrap();
        assert_eq!(searched["result"]["isError"], false);
        let parse: Value = serde_json::from_str(&responses[3]).unwrap();
        assert_eq!(parse["error"]["code"], -32700);
    }

    #[test]
    fn an_edited_input_conflicts_and_a_failure_returns_its_log() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), b"one").unwrap();
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: root.path().display().to_string(),
            source_revision: Some("abc".to_owned()),
        };
        let mut record = operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "test", "flake.nix"),
        );
        record.inputs = vec!["lib.rs".to_owned()];
        let records = vec![record];
        let dir = tempfile::tempdir().unwrap();
        let launch = Launch::Command(vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo boom; exit 7".into(),
        ]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "edit"}}),
            &records,
            root.path(),
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        std::fs::write(root.path().join("lib.rs"), b"two").unwrap();
        let conflict = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "edit"}}),
            &records,
            root.path(),
            dir.path(),
            true,
            &launch,
        );
        assert!(conflict.unwrap_err().contains("conflicts"));
        let run_id = started["run_id"].as_str().unwrap();
        let mut log = json!({});
        for _ in 0..20 {
            log = invoke(
                &json!({"name": "runs_get_logs", "arguments": {"run_id": run_id, "limit": 100}}),
                &records,
                root.path(),
                dir.path(),
                true,
                &launch,
            )
            .unwrap();
            if log["log"].as_str().unwrap_or("").contains("boom") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(log["log"].as_str().unwrap_or("").contains("boom"));
        let missing = records.clone();
        let mut broken = missing;
        broken[0].inputs = vec!["missing.rs".to_owned()];
        let refused = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "missing"}}),
            &broken,
            root.path(),
            dir.path(),
            true,
            &launch,
        );
        assert!(refused.unwrap_err().contains("cannot be read"));
        forget_run(run_id);
    }

    #[test]
    fn cancel_stops_the_running_process() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let launch = Launch::Command(vec!["/bin/sleep".into(), "30".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "sleep"}}),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        let run_id = started["run_id"].as_str().unwrap();
        let cancelled = invoke(
            &json!({"name": "runs_cancel", "arguments": {"run_id": run_id}}),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(cancelled["cancelled"], true);
        forget_run(run_id);
    }

    #[test]
    fn a_failed_admission_does_not_launch_a_second_process() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let launch = Launch::Command(vec!["/no/such/nxr-command".into()]);
        let first = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "crash"}}),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            true,
            &launch,
        );
        assert!(first.is_err());
        let again = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "crash"}}),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(again["started"], false);
        assert_eq!(again["status"], "reconciliation-required");
    }
}
