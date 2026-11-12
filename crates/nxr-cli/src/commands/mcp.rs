//! Local stdio MCP server for the operation catalog.
//!
//! The workspace is the checkout this process was launched in. A tool argument
//! cannot select another checkout. Execution stays behind `execution_allowed`.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use nxr_nix::OptionalNixFlags;
use std::collections::BTreeMap;

use nxr_task::{
    OperationKind, OperationRecord, ProjectExecutionPolicy, admit, apply_project_policy,
    bind_supervisor, cancel, declared_input_digests, execution_fingerprint_with_inputs, inspect,
    mark_admitting, mark_reconciliation, validate_policy_parameters,
};
use serde_json::{Value, json};

use crate::commands::affected::{AffectedPathSources, select_for_flake};
use crate::commands::op::load_records;
use crate::commands::session::{
    LocalBudget, SessionStart, cancel_on, inspect_on, list_on, start_on,
};
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
            &nxr_core::daemon_socket_path(),
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
    supervisor_socket: &Path,
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
        supervisor_socket,
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
    supervisor_socket: &Path,
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
            "serverInfo": {
                "name": "nxr",
                "version": "0.1.0",
                "executable": std::env::current_exe().map(|path| path.display().to_string()).unwrap_or_default()
            }
        }),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => match call_tool(
            &params,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            supervisor_dir,
            supervisor_socket,
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
            "operations_suggest",
            "Rank operations for a natural-language query",
            &["query"],
            json!({
                "query": {"type": "string", "description": "What the caller wants to do"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Changed repository-relative paths"}
            }),
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
                "idempotency_key": {"type": "string", "description": "Caller key for this attempt"},
                "parameters": {"type": "object", "description": "Parameter names listed for this operation id"}
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
    supervisor_socket: &Path,
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
            let visible = policy_view(records, workspace_root, trusted)?;
            let matched = nxr_task::search(&visible, query);
            Ok(json!(matched))
        }
        "operations_suggest" => {
            let query = arguments.get("query").and_then(Value::as_str).unwrap_or("");
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
            let visible = policy_view(records, workspace_root, trusted)?;
            Ok(json!({
                "query": query,
                "results": nxr_task::suggest(&visible, query, workspace_id, &paths)
            }))
        }
        "operations_describe" => {
            let id = required_id(&arguments)?;
            let visible = policy_view(records, workspace_root, trusted)?;
            nxr_task::describe(&visible, id)
                .map(|record| json!(record))
                .ok_or_else(|| format!("unknown operation {id}"))
        }
        "operations_plan" => {
            let id = required_id(&arguments)?;
            let visible = policy_view(records, workspace_root, trusted)?;
            let record = nxr_task::describe(&visible, id)
                .ok_or_else(|| format!("unknown operation {id}"))?;
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
        "runs_get_logs" => read_run_log(
            &arguments,
            run_dir,
            supervisor_dir,
            supervisor_socket,
            workspace_id,
        ),
        "operations_start" => start(
            &arguments,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            supervisor_dir,
            supervisor_socket,
            trusted,
            flake,
            nix_override,
            nix_flags,
            runner,
            launch,
            reload,
        ),
        "runs_cancel" => cancel_run(
            run_dir,
            supervisor_dir,
            supervisor_socket,
            workspace_id,
            &arguments,
        ),
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
    _supervisor_dir: &Path,
    supervisor_socket: &Path,
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
    let policy = ProjectExecutionPolicy::load(workspace_root)?;
    let stored = nxr_task::describe(&fresh, id).ok_or_else(|| format!("unknown operation {id}"))?;
    if stored.workspace_id != workspace_id {
        return Err("operation is not in the bound workspace".to_owned());
    }
    let record = if trusted {
        apply_project_policy(stored, &policy)
    } else {
        stored.clone()
    };
    if !record.execution_allowed {
        return Err("operation is not executable".to_owned());
    }
    let parameters = parameter_map(arguments)?;
    validate_policy_parameters(&policy, &record.operation_id, &parameters)?;
    let digests = declared_input_digests(workspace_root, &record.inputs)?;
    let fingerprint = execution_fingerprint_with_inputs(&record, &digests, &parameters);
    match admit(run_dir, key, &record, &fingerprint)? {
        nxr_task::Admission::Reserved(_) => {
            mark_admitting(run_dir, key)?;
            let (command, cwd) = command_for(launch, &record, &parameters, workspace_root)?;
            let admission = admission_id(workspace_id, workspace_root, key);
            match start_on(
                supervisor_socket,
                &SessionStart {
                    label: key.to_owned(),
                    command,
                    remote: None,
                    attempt_id: None,
                    cpu: 0,
                    memory_mib: 0,
                    cwd: Some(cwd),
                    admission_id: Some(admission.clone()),
                },
                LocalBudget {
                    cpu: u32::MAX,
                    memory_mib: u64::MAX,
                },
                &BTreeMap::new(),
                runner,
            ) {
                Ok(run_id) => {
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
        nxr_task::Admission::Existing(reservation) if reservation.status == "admitting" => {
            let admission = admission_id(workspace_id, workspace_root, key);
            if let Some(run_id) = owned_run_id(supervisor_socket, &admission) {
                bind_supervisor(run_dir, key, &run_id)?;
                return Ok(json!({
                    "run_id": run_id,
                    "started": false,
                    "replayed": true,
                    "status": "started"
                }));
            }
            Ok(json!({
                "run_id": reservation.run_id,
                "started": false,
                "replayed": true,
                "status": reservation.status
            }))
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

fn policy_view(
    records: &[OperationRecord],
    root: &Path,
    trusted: bool,
) -> Result<Vec<OperationRecord>, String> {
    if !trusted {
        return Ok(records.to_vec());
    }
    let policy = ProjectExecutionPolicy::load(root)?;
    Ok(records
        .iter()
        .map(|record| apply_project_policy(record, &policy))
        .collect())
}

fn admission_id(workspace_id: &str, checkout: &Path, key: &str) -> String {
    nxr_core::digest_bytes(format!("{workspace_id}\n{}\n{key}", checkout.display()).as_bytes())
}

fn parameter_map(arguments: &Value) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = arguments.get("parameters") else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "parameters must be an object".to_owned())?;
    let mut parameters = BTreeMap::new();
    for (name, item) in object {
        let text = item
            .as_str()
            .ok_or_else(|| format!("parameter `{name}` must be a string"))?;
        parameters.insert(name.clone(), text.to_owned());
    }
    Ok(parameters)
}

fn command_for(
    launch: &Launch,
    record: &OperationRecord,
    parameters: &BTreeMap<String, String>,
    root: &Path,
) -> Result<(Vec<String>, PathBuf), String> {
    let cwd = working_directory(root, &record.working_directory)?;
    match launch {
        Launch::Command(argv) => Ok((argv.clone(), cwd)),
        Launch::Task => {
            let executable = std::env::current_exe().map_err(|err| err.to_string())?;
            let verb = match record.kind {
                OperationKind::App => "run",
                OperationKind::Task => "task",
                _ => return Err("operation is not executable".to_owned()),
            };
            let mut command = vec![
                executable.display().to_string(),
                verb.to_owned(),
                record.name.clone(),
            ];
            if !parameters.is_empty() {
                command.push("--".to_owned());
                for (name, value) in parameters {
                    command.push(format!("{name}={value}"));
                }
            }
            Ok((command, cwd))
        }
    }
}

fn working_directory(root: &Path, working: &str) -> Result<PathBuf, String> {
    if working.is_empty() || working == "." {
        return Ok(root.to_path_buf());
    }
    if working.starts_with('/') || working.split(['/', '\\']).any(|part| part == "..") {
        return Err("working directory is not inside the workspace".to_owned());
    }
    Ok(root.join(working))
}

fn owned_run_id(socket: &Path, admission: &str) -> Option<String> {
    if admission.is_empty() {
        return None;
    }
    let runs = list_on(socket).ok()?;
    runs.as_array().and_then(|rows| {
        rows.iter().find_map(|row| {
            (row.get("admission_id").and_then(Value::as_str) == Some(admission))
                .then(|| row.get("id").and_then(Value::as_str).map(str::to_owned))
                .flatten()
        })
    })
}

fn supervisor_record(socket: &Path, dir: &Path, run_id: &str) -> Result<Value, String> {
    inspect_on(socket, run_id).or_else(|_| nxr_core::execution_owner::inspect(dir, run_id))
}

fn read_run_log(
    arguments: &Value,
    run_dir: &Path,
    supervisor_dir: &Path,
    supervisor_socket: &Path,
    workspace_id: &str,
) -> Result<Value, String> {
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    nxr_core::validate_run_id(run_id).map_err(|err| err.to_string())?;
    let found = inspect(run_dir, run_id, workspace_id)?;
    if found.is_none() {
        return Err("run is not in this workspace".to_owned());
    }
    let record = supervisor_record(supervisor_socket, supervisor_dir, run_id)?;
    let log_path = record
        .get("log_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "log is not available".to_owned())?;
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4096)
        .min(65536) as usize;
    let (log, next) = read_log_slice(Path::new(log_path), offset, limit)?;
    let durable = nxr_core::load_durable_run(run_id).ok();
    Ok(json!({
        "run_id": run_id,
        "status": durable.as_ref().map(|row| format!("{:?}", row.status)).or_else(|| found.as_ref().map(|row| row.status.clone())),
        "exit_code": durable.as_ref().and_then(|row| row.exit_code),
        "log": log,
        "next_offset": next
    }))
}

fn cancel_run(
    run_dir: &Path,
    supervisor_dir: &Path,
    supervisor_socket: &Path,
    workspace_id: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    nxr_core::validate_run_id(run_id).map_err(|err| err.to_string())?;
    let owned = inspect(run_dir, run_id, workspace_id)?;
    if owned.is_none() {
        return Ok(json!({
            "cancellation_requested": false,
            "process_cancelled": false,
            "process_terminal": false,
            "reservation_cancelled": false
        }));
    }
    let before = supervisor_record(supervisor_socket, supervisor_dir, run_id).ok();
    let was_running = before
        .as_ref()
        .and_then(|record| record.get("status").and_then(Value::as_str))
        == Some("running");
    if !was_running {
        let reservation_cancelled = cancel(run_dir, run_id, workspace_id)?;
        return Ok(json!({
            "cancellation_requested": false,
            "process_cancelled": false,
            "process_terminal": true,
            "reservation_cancelled": reservation_cancelled
        }));
    }
    if cancel_on(supervisor_socket, run_id).is_err() {
        return Ok(json!({
            "cancellation_requested": true,
            "process_cancelled": false,
            "process_terminal": false,
            "reservation_cancelled": false
        }));
    }
    let after = supervisor_record(supervisor_socket, supervisor_dir, run_id)?;
    let still_running = after.get("status").and_then(Value::as_str) == Some("running");
    if still_running {
        return Ok(json!({
            "cancellation_requested": true,
            "process_cancelled": false,
            "process_terminal": false,
            "reservation_cancelled": false
        }));
    }
    let reservation_cancelled = cancel(run_dir, run_id, workspace_id)?;
    Ok(json!({
        "cancellation_requested": true,
        "process_cancelled": true,
        "process_terminal": true,
        "reservation_cancelled": reservation_cancelled
    }))
}

fn read_log_slice(path: &Path, offset: u64, limit: usize) -> Result<(String, u64), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|_| "log is not available".to_owned())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| err.to_string())?;
    let mut buffer = vec![0; limit];
    let read = file.read(&mut buffer).map_err(|err| err.to_string())?;
    buffer.truncate(read);
    Ok((
        String::from_utf8_lossy(&buffer).into_owned(),
        offset.saturating_add(read as u64),
    ))
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
        "operations_suggest" => &["query", "paths"],
        "operations_describe" | "operations_plan" => &["id"],
        "runs_inspect" | "runs_cancel" => &["run_id"],
        "workspace_affected" => &["paths"],
        "operations_start" => &["id", "idempotency_key", "parameters"],
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

    fn boot(dir: &std::path::Path) -> std::path::PathBuf {
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::Duration;

        use nxr_core::{DaemonState, serve, try_connect};

        let socket = dir.join("nxrd.sock");
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let serve_socket = socket.clone();
        thread::spawn(move || {
            let _ = serve(&serve_socket, state);
        });
        for _ in 0..50 {
            if try_connect(&socket).is_ok() {
                return socket;
            }
            thread::sleep(Duration::from_millis(20));
        }
        socket
    }

    fn invoke(
        params: &Value,
        records: &[OperationRecord],
        root: &std::path::Path,
        dir: &std::path::Path,
        socket: &std::path::Path,
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
            socket,
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
            std::path::Path::new("/tmp/missing.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(described["operation_id"], "lattice:task:test");
        assert_eq!(described["execution_allowed"], true);
        let dir = tempfile::tempdir().unwrap();
        let socket = boot(dir.path());
        let root = dir.path();
        let launch = Launch::Command(vec!["/bin/echo".into(), "ok".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "once"}}),
            &records,
            root,
            dir.path(),
            &socket,
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
            &socket,
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
            &socket,
            true,
            &launch,
        );
        assert!(refused.is_err());
        let foreign = invoke(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:test", "workspace_root": "/other"}}),
            &records,
            root,
            dir.path(),
            &socket,
            true,
            &launch,
        );
        assert!(foreign.is_err());
        let untrusted = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "other"}}),
            &records,
            root,
            dir.path(),
            &socket,
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
                &dir.path().join("nxrd.sock"),
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
        let tools = listed["result"]["tools"].as_array().unwrap();
        let start = tools
            .iter()
            .find(|tool| tool["name"] == "operations_start")
            .unwrap();
        let schema = &start["inputSchema"];
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
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), b"one").unwrap();
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: root.path().display().to_string(),
            source_revision: Some("abc".to_owned()),
        };
        let mut record = operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "test", "flake.nix"),
        );
        record.inputs = vec!["src".to_owned()];
        let records = vec![record];
        let dir = tempfile::tempdir().unwrap();
        let socket = boot(dir.path());
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
            &socket,
            true,
            &launch,
        )
        .unwrap();
        std::fs::write(root.path().join("src/lib.rs"), b"two").unwrap();
        let conflict = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "edit"}}),
            &records,
            root.path(),
            dir.path(),
            &socket,
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
                &socket,
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
            &socket,
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
        let socket = boot(dir.path());
        let root = dir.path();
        let launch = Launch::Command(vec!["/bin/sleep".into(), "30".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "sleep"}}),
            &records,
            root,
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        let run_id = started["run_id"].as_str().unwrap();
        let cancelled = invoke(
            &json!({"name": "runs_cancel", "arguments": {"run_id": run_id}}),
            &records,
            root,
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(cancelled["process_cancelled"], true);
        assert_eq!(cancelled["reservation_cancelled"], true);
        forget_run(run_id);
    }

    #[test]
    fn a_failed_admission_does_not_launch_a_second_process() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let socket = boot(dir.path());
        let root = dir.path();
        let launch = Launch::Command(vec!["/no/such/nxr-command".into()]);
        let first = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "crash"}}),
            &records,
            root,
            dir.path(),
            &socket,
            true,
            &launch,
        );
        assert!(first.is_err());
        let again = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "crash"}}),
            &records,
            root,
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(again["started"], false);
        assert_eq!(again["status"], "reconciliation-required");
    }

    #[test]
    fn an_app_is_launched_with_run_and_an_interrupted_bind_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: root.path().display().to_string(),
            source_revision: None,
        };
        let records = vec![operation_record(
            &workspace,
            &Declaration::named(OperationKind::App, "test", "flake.nix"),
        )];
        let (command, _) =
            command_for(&Launch::Task, &records[0], &BTreeMap::new(), root.path()).unwrap();
        assert_eq!(command[1], "run");
        assert_ne!(command[0], "nxr");
        let dir = tempfile::tempdir().unwrap();
        let socket = boot(dir.path());
        let launch = Launch::Command(vec!["/bin/echo".into(), "ok".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:app:test", "idempotency_key": "app"}}),
            &records,
            root.path(),
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        let run_id = started["run_id"].as_str().unwrap();
        nxr_task::mark_bind_incomplete(dir.path(), "app").unwrap();
        let again = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:app:test", "idempotency_key": "app"}}),
            &records,
            root.path(),
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(again["run_id"], run_id);
        assert_eq!(again["started"], false);
        let listed = list_on(&socket).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        forget_run(run_id);
    }

    #[test]
    fn project_policy_authorizes_one_operation_id() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".nxr")).unwrap();
        std::fs::write(
            root.path().join(".nxr/execution-policy.toml"),
            "[[operation]]\nid = \"lattice:task:rust-test-pkgs\"\nparameters = [\"package\"]\n\n[[operation]]\nid = \"lattice:task:reset-db\"\n",
        )
        .unwrap();
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: root.path().display().to_string(),
            source_revision: None,
        };
        let records = vec![
            operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "rust-test-pkgs", "flake.nix"),
            ),
            operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "reset-db", "flake.nix"),
            ),
        ];
        let described = invoke(
            &json!({"name": "operations_describe", "arguments": {"id": "lattice:task:rust-test-pkgs"}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(described["execution_allowed"], true);
        assert_eq!(described["policy_source"], "project");
        let searched = invoke(
            &json!({"name": "operations_search", "arguments": {"query": "rust-test-pkgs"}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(searched[0]["execution_allowed"], true);
        let suggested = invoke(
            &json!({"name": "operations_suggest", "arguments": {"query": "rust packages"}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        let approved = suggested["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["operation_id"] == "lattice:task:rust-test-pkgs")
            .unwrap();
        assert_eq!(approved["execution_allowed"], true);
        let planned = invoke(
            &json!({"name": "operations_plan", "arguments": {"id": "lattice:task:reset-db"}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(planned["execution_allowed"], false);
        let refused = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:reset-db", "idempotency_key": "db", "parameters": {"package": "x"}}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        );
        assert!(refused.unwrap_err().contains("not executable"));
        let extra = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:rust-test-pkgs", "idempotency_key": "pkgs", "parameters": {"shell": "bash"}}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        );
        assert!(extra.unwrap_err().contains("not authorized"));
    }

    #[test]
    fn two_checkouts_with_the_same_key_reconnect_to_their_own_runs() {
        let daemon = tempfile::tempdir().unwrap();
        let socket = boot(daemon.path());
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let launch = Launch::Command(vec!["/bin/echo".into(), "ok".into()]);
        let start_one = |root: &std::path::Path| {
            let workspace = WorkspaceInstance {
                workspace_id: "lattice".to_owned(),
                workspace_root: root.display().to_string(),
                source_revision: None,
            };
            let records = vec![operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "test", "flake.nix"),
            )];
            invoke(
                &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "shared"}}),
                &records,
                root,
                root,
                &socket,
                true,
                &launch,
            )
            .unwrap()
        };
        let started_first = start_one(first.path());
        let started_second = start_one(second.path());
        let first_id = started_first["run_id"].as_str().unwrap().to_owned();
        let second_id = started_second["run_id"].as_str().unwrap().to_owned();
        assert_ne!(first_id, second_id);
        nxr_task::mark_bind_incomplete(first.path(), "shared").unwrap();
        nxr_task::mark_bind_incomplete(second.path(), "shared").unwrap();
        let again_first = start_one(first.path());
        let again_second = start_one(second.path());
        assert_eq!(again_first["run_id"], first_id);
        assert_eq!(again_second["run_id"], second_id);
        assert_eq!(again_first["started"], false);
        assert_eq!(again_second["started"], false);
        forget_run(&first_id);
        forget_run(&second_id);
    }

    #[test]
    fn a_failed_cancel_does_not_mark_a_live_process_finished() {
        let dir = tempfile::tempdir().unwrap();
        let socket = boot(dir.path());
        let records = records();
        let launch = Launch::Command(vec!["/bin/sleep".into(), "30".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "live"}}),
            &records,
            dir.path(),
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        let run_id = started["run_id"].as_str().unwrap();
        let path = dir.path().join("supervisor").join(format!("{run_id}.json"));
        let mut stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        stored["attempt_id"] = json!("attempt-live");
        std::fs::write(&path, serde_json::to_string(&stored).unwrap()).unwrap();
        let cancelled = invoke(
            &json!({"name": "runs_cancel", "arguments": {"run_id": run_id}}),
            &records,
            dir.path(),
            dir.path(),
            &socket,
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(cancelled["cancellation_requested"], true);
        assert_eq!(cancelled["process_cancelled"], false);
        assert_eq!(cancelled["reservation_cancelled"], false);
        let still = inspect_on(&socket, run_id).unwrap();
        assert_eq!(still["status"], "running");
        nxr_core::execution_owner::cancel(&dir.path().join("supervisor"), run_id, true).unwrap();
        forget_run(run_id);
    }
}
