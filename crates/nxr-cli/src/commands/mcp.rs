//! Local stdio MCP server for the operation catalog.
//!
//! The workspace is the checkout this process was launched in. A tool argument
//! cannot select another checkout. Execution stays behind `execution_allowed`.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use camino::Utf8Path;
use nxr_affected::{NodeStatus, git_working_tree_changes};
use nxr_nix::OptionalNixFlags;
use std::collections::BTreeMap;

use nxr_task::{
    ChildObservation, ContextIdentity, OperationKind, OperationRecord, ProjectExecutionPolicy,
    ReceiptEvent, ShellProbe, StoredReceipt, ValidationPlanInput, admit, apply_project_policy,
    attach_events, bind_supervisor, cache_reason, cancel, child_probe_script,
    declared_input_digests, execution_fingerprint_with_inputs, inspect, mark_admitting,
    mark_reconciliation, parse_child_probe, parse_run_log, read_leaf_receipt, read_receipt,
    static_receipt, transition, validate_policy_parameters, validation_plan, verified_receipt,
    write_receipt,
};
use serde_json::{Value, json};

use crate::commands::affected::{AffectedPathSources, select_for_flake};
use crate::commands::op::load_records;
use crate::commands::session::{
    LocalBudget, SessionStart, cancel_on, inspect_on, list_on, start_on,
};
#[cfg(test)]
use crate::output_options::{ColorWhen, LogFormat, OutputOptions};
use crate::runner_output::RunnerOutput;

#[cfg(test)]
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
    #[allow(dead_code)]
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
    let run_dir = reservation_dir(&workspace_id, &workspace_root)?;
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
    message.get("id")?;
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
            "operations_plan_batch",
            "Plan affected operations without starting them. Coverage is empty, complete, partial, or unknown, and the result does not authorize execution",
            &["paths"],
            json!({"paths": {"type": "array", "items": {"type": "string"}, "description": "Changed repository-relative paths"}}),
        ),
        tool(
            "runs_inspect",
            "Read one run in this workspace",
            &["run_id"],
            json!({"run_id": {"type": "string", "description": "Supervisor run id"}}),
        ),
        tool(
            "runs_get_receipt",
            "Read the execution receipt and the stored supervisor events for one run",
            &["id", "run_id"],
            json!({
                "id": {"type": "string", "description": "Portable operation id"},
                "run_id": {"type": "string", "description": "Supervisor run id"}
            }),
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
            "operations_start_remote",
            "Start one operation on NixPlane. Local start permission does not grant this.",
            &["id", "idempotency_key"],
            json!({
                "id": {"type": "string", "description": "Portable operation id"},
                "idempotency_key": {"type": "string", "description": "Caller key for this attempt"},
                "spec": {"type": "object", "description": "Execution spec document"},
                "source_set": {"type": "object", "description": "Source set document"},
                "dirty_manifest": {"description": "Dirty manifest object or path string"},
                "source_locator": {"type": "string", "description": "Locator for a git source set"},
                "parameters": {"type": "object", "description": "Parameter names listed for this operation id"}
            }),
        ),
        tool(
            "runs_get_diagnostics",
            "Classify an existing run log. This does not call a model or create a second store",
            &["run_id"],
            json!({"run_id": {"type": "string", "description": "Supervisor run id"}}),
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
        "operations_plan_batch" => plan_batch(
            &arguments,
            records,
            workspace_id,
            workspace_root,
            flake,
            nix_override,
            nix_flags,
            runner,
        ),
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
        "runs_get_receipt" => run_receipt(
            &arguments,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            supervisor_dir,
            trusted,
        ),
        "runs_get_diagnostics" => run_diagnostics(&arguments, supervisor_dir),
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
        "operations_start_remote" => start_remote(
            &arguments,
            records,
            workspace_id,
            workspace_root,
            run_dir,
            trusted,
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
    let stored = nxr_task::describe(&fresh, id).ok_or_else(|| format!("unknown operation {id}"))?;
    if stored.workspace_id != workspace_id {
        return Err("operation is not in the bound workspace".to_owned());
    }
    let gate = policy_gate(workspace_root)?;
    let record = if trusted {
        apply_project_policy(stored, &gate.applied)
    } else {
        stored.clone()
    };
    if !record.execution_allowed {
        if trusted && !gate.approved && gate.file.contains(id) {
            return Err("policy approval required".to_owned());
        }
        return Err("operation is not executable".to_owned());
    }
    let parameters = parameter_map(arguments)?;
    validate_policy_parameters(&gate.applied, &record.operation_id, &parameters)?;
    let positional = gate
        .applied
        .ordered_arguments(&record.operation_id, &parameters);
    let digests = declared_input_digests(workspace_root, &record.inputs)?;
    let fingerprint = execution_fingerprint_with_inputs(&record, &digests, &parameters);
    let parameter_text = canonical_parameters(&parameters);
    if let Some(reused) = reused_qualification(
        _supervisor_dir,
        workspace_root,
        &record.operation_id,
        &fingerprint,
        &parameter_text,
    ) {
        return Ok(reused);
    }
    match admit(run_dir, key, &record, &fingerprint)? {
        nxr_task::Admission::Reserved(_) => {
            mark_admitting(run_dir, key)?;
            let (command, cwd) = command_for(launch, &record, &positional, workspace_root)?;
            let admission = admission_id(workspace_id, workspace_root, key)?;
            let resources = admitted_output_resources(workspace_root, &command)?;
            let output_dirs: Vec<String> = resources
                .iter()
                .map(|resource| nxr_core::execution_owner::canonical_target_dir(&resource.path))
                .collect();
            let target_dir = output_dirs.first().cloned();
            let parallelism = std::thread::available_parallelism()
                .map(|count| u32::try_from(count.get()).unwrap_or(1))
                .unwrap_or(1);
            let host = nxr_core::admission_budget::host_budget(
                std::env::var("NXR_LOCAL_CPU").ok().as_deref(),
                std::env::var("NXR_LOCAL_MEMORY_MIB").ok().as_deref(),
                parallelism,
            );
            let heavy = command_is_heavy_cargo(&command);
            let reservation = nxr_core::admission_budget::reservation_for(0, 0, heavy);
            let probe_script = if should_probe_child(&command) {
                Some(child_probe_script().to_owned())
            } else {
                None
            };
            let identity = execution_identity(
                &record.operation_id,
                &parameter_text,
                &fingerprint,
                probe_script.as_deref(),
                cwd.as_path(),
            );
            let digest = nxr_core::digest_bytes(key.as_bytes());
            let coalesce_run = format!("run-{}", &digest[..16]);
            let coalesce = nxr_task::attach_observer(
                run_dir,
                &nxr_task::CoalesceRequest {
                    observer_id: key.to_owned(),
                    run_id: coalesce_run,
                    identity: identity.clone(),
                    resources: output_dirs.clone(),
                    verified: identity_verified(&identity),
                },
            )?;
            let reserved_run = match coalesce {
                nxr_task::CoalesceOutcome::Attach { run_id } => {
                    let running = supervisor_record(supervisor_socket, _supervisor_dir, &run_id)
                        .ok()
                        .and_then(|row| {
                            row.get("status").and_then(Value::as_str).map(str::to_owned)
                        })
                        == Some("running".to_owned());
                    if running {
                        let _ =
                            nxr_core::execution_owner::note(_supervisor_dir, &run_id, "attached");
                        bind_supervisor(run_dir, key, &run_id)?;
                        return Ok(json!({
                            "run_id": run_id,
                            "started": false,
                            "replayed": true,
                            "status": "attached"
                        }));
                    }
                    let _ = nxr_task::finish_coalesce(run_dir, &run_id);
                    let _ = nxr_task::detach_observer_id(run_dir, &run_id, key);
                    let retry_id = format!(
                        "run-{}",
                        &nxr_core::digest_bytes(format!("{key}:{run_id}").as_bytes())[..16]
                    );
                    match nxr_task::attach_observer(
                        run_dir,
                        &nxr_task::CoalesceRequest {
                            observer_id: key.to_owned(),
                            run_id: retry_id,
                            identity: identity.clone(),
                            resources: output_dirs.clone(),
                            verified: identity_verified(&identity),
                        },
                    )? {
                        nxr_task::CoalesceOutcome::Start { run_id } => Some(run_id),
                        nxr_task::CoalesceOutcome::Attach { run_id } => {
                            let _ = nxr_core::execution_owner::note(
                                _supervisor_dir,
                                &run_id,
                                "attached",
                            );
                            bind_supervisor(run_dir, key, &run_id)?;
                            return Ok(json!({
                                "run_id": run_id,
                                "started": false,
                                "replayed": true,
                                "status": "attached"
                            }));
                        }
                        nxr_task::CoalesceOutcome::Conflict { reason } => return Err(reason),
                        nxr_task::CoalesceOutcome::Unverified => None,
                    }
                }
                nxr_task::CoalesceOutcome::Conflict { reason } => return Err(reason),
                nxr_task::CoalesceOutcome::Start { run_id } => Some(run_id),
                nxr_task::CoalesceOutcome::Unverified => None,
            };
            let qualify = record.operation_id.ends_with(":task:test");
            match start_on(
                supervisor_socket,
                &SessionStart {
                    label: key.to_owned(),
                    command,
                    remote: None,
                    attempt_id: None,
                    cpu: reservation.cpu,
                    memory_mib: reservation.memory_mib,
                    cwd: Some(cwd),
                    admission_id: Some(admission.clone()),
                    target_dir,
                    operation_id: Some(record.operation_id.clone()),
                    workspace_id: Some(workspace_id.to_owned()),
                    output_dirs: output_dirs.clone(),
                    probe_script,
                    run_id: reserved_run.clone(),
                    source_digest: Some(fingerprint.clone()),
                    leaf_id: Some("command".to_owned()),
                    qualify,
                    parameters: Some(parameter_text.clone()),
                },
                LocalBudget {
                    cpu: host.cpu,
                    memory_mib: host.memory_mib,
                },
                &BTreeMap::new(),
                runner,
            ) {
                Ok(run_id) => {
                    persist_child_observation(
                        _supervisor_dir,
                        &run_id,
                        &record.operation_id,
                        workspace_id,
                        &fingerprint,
                        "command",
                    );
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
                    if let Some(run_id) = reserved_run.as_deref() {
                        let _ = nxr_task::finish_coalesce(run_dir, run_id);
                    }
                    let _ = mark_reconciliation(run_dir, key);
                    Err(error)
                }
            }
        }
        nxr_task::Admission::Existing(reservation) if reservation.status == "admitting" => {
            let admission = admission_id(workspace_id, workspace_root, key)?;
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

fn start_remote(
    arguments: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    trusted: bool,
) -> Result<Value, String> {
    let id = required_id(arguments)?;
    let key = arguments
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if key.is_empty() {
        return Err("idempotency_key is required".to_owned());
    }
    let gate = policy_gate(workspace_root)?;
    let Some(spec) = arguments.get("spec").filter(|value| value.is_object()) else {
        return Err("execution spec is required".to_owned());
    };
    let Some(source_set) = arguments
        .get("source_set")
        .filter(|value| value.is_object())
    else {
        return Err("execution spec is required".to_owned());
    };
    let visible = policy_view(records, workspace_root, trusted)?;
    let record =
        nxr_task::describe(&visible, id).ok_or_else(|| format!("unknown operation {id}"))?;
    let validated = crate::commands::remote_start::validate_remote_submission(
        &gate.applied,
        id,
        &record.operation_id,
        &record.workspace_id,
        workspace_id,
        spec,
        source_set,
    )?;
    if validated.operation_id.is_empty() {
        return Err("remote execution is not authorized".to_owned());
    }
    let mut body = json!({
        "spec": crate::commands::remote_start::spec_with_idempotency(&validated.spec, key),
        "source_set": source_set,
    });
    if let Some(dirty) = arguments.get("dirty_manifest") {
        body["dirty_manifest"] = dirty.clone();
    }
    if let Some(locator) = arguments.get("source_locator").and_then(Value::as_str) {
        body["source_locator"] = json!(locator);
    }
    let bin = std::env::var("NIXPLANE").unwrap_or_else(|_| "nixplane".to_owned());
    let state_dir = std::env::var("NIXPLANE_STATE_DIR").ok();
    let transport = crate::commands::nixplane_transport::NixplaneCliTransport { bin, state_dir };
    nxr_task::submit_and_bind(
        run_dir,
        key,
        &nxr_task::remote_run_id(key),
        &record.operation_id,
        &validated.source_set_id,
        &transport,
        &body,
    )
}

fn run_receipt(
    arguments: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    run_dir: &Path,
    supervisor_dir: &Path,
    trusted: bool,
) -> Result<Value, String> {
    let id = required_id(arguments)?;
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.is_empty())
        .ok_or_else(|| "run_id is required".to_owned())?;
    let visible = policy_view(records, workspace_root, trusted)?;
    let record =
        nxr_task::describe(&visible, id).ok_or_else(|| format!("unknown operation {id}"))?;
    if let Some(remote) = nxr_task::remote_attempt_for_run(run_dir, run_id)? {
        return remote_run_receipt(arguments, run_dir, workspace_id, record, remote);
    }
    let stored_run = nxr_core::execution_owner::inspect(supervisor_dir, run_id)?;
    let stored_operation = stored_run
        .get("operation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let stored_workspace = stored_run
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    nxr_core::receipt_binding_matches(
        stored_operation,
        stored_workspace,
        &record.operation_id,
        workspace_id,
    )?;
    let source_digest = stored_run
        .get("source_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let leaf_id = stored_run
        .get("leaf_id")
        .and_then(Value::as_str)
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or("command");
    if !source_digest.is_empty() {
        persist_child_observation(
            supervisor_dir,
            run_id,
            &record.operation_id,
            workspace_id,
            source_digest,
            leaf_id,
        );
    }
    let observed = stored_child_observation(supervisor_dir, run_id);
    let (mut receipt, context, verified) = if let Some(observation) = observed {
        let probe = shell_probe_from_observation(&observation);
        let receipt = verified_receipt(record, &probe)?;
        let context = ContextIdentity {
            rustc_version: observation.rustc_version,
            wrapper_name: observation.wrapper_path,
            target_dir: observation.target_directory,
            cache_mode: observation.cache_identity,
        };
        (receipt, context, true)
    } else {
        (static_receipt(record), unverified_context(), false)
    };
    let reason = cache_reason(None, &context, false, false, verified);
    let stored = nxr_core::execution_owner::events(supervisor_dir, run_id, 0)?;
    let events = stored
        .as_array()
        .ok_or_else(|| "supervisor events are not a list".to_owned())?;
    let mut copied = Vec::with_capacity(events.len());
    for event in events {
        let cursor = event
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or_else(|| "supervisor event is missing a cursor".to_owned())?;
        let message = event
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        copied.push(ReceiptEvent { cursor, message });
    }
    attach_events(&mut receipt, copied);
    let mut value = serde_json::to_value(&receipt).map_err(|err| err.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert("cache_reason".to_owned(), json!(reason));
        if let Ok(tallies) = nxr_core::execution_owner::tallies(supervisor_dir) {
            let report =
                nxr_task::economics::economics_report(&nxr_task::economics::EconomicsInput {
                    requested: tallies.requested,
                    unique_executions: tallies.unique_executions,
                    coalesced: tallies.attached,
                    reused_qualifications: tallies.reused,
                    compiler_cache_hits: None,
                    wall_time_ms: None,
                    queue_wait_ms: None,
                    peak_disk_bytes: None,
                    artifacts_written: None,
                    artifacts_transferred: None,
                    test_compile_ms: None,
                    test_execute_ms: None,
                    measured_saved_ms: None,
                    estimated_saved_ms: None,
                });
            let economics_path = supervisor_dir
                .join("provenance")
                .join(format!("{run_id}.economics.json"));
            if let Some(parent) = economics_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&economics_path, report.to_string());
            object.insert("economics_report".to_owned(), report);
        }
        if let Some(remote) = nxr_task::read_remote_receipt(run_dir, run_id)? {
            object.insert(
                "remote_receipt".to_owned(),
                serde_json::to_value(remote).map_err(|err| err.to_string())?,
            );
        }
    }
    Ok(value)
}

fn remote_run_receipt(
    arguments: &Value,
    run_dir: &Path,
    workspace_id: &str,
    record: &OperationRecord,
    remote: nxr_task::RemoteAttempt,
) -> Result<Value, String> {
    if remote.operation_id != record.operation_id || record.workspace_id != workspace_id {
        return Err("remote attempt does not match the operation".to_owned());
    }
    if let Some(source_set_id) = arguments.get("source_set_id").and_then(Value::as_str)
        && source_set_id != remote.source_set_id
    {
        return Err("remote attempt does not match the source set".to_owned());
    }
    if let Some(lease) = arguments.get("lease_generation").and_then(Value::as_u64)
        && remote.lease_generation != Some(lease)
    {
        return Err("lease does not match the attempt".to_owned());
    }
    let run_id = remote.run_id.clone();
    if let Some(output_name) = arguments
        .get("output_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        && let Some(attempt_id) = remote.attempt_id.clone()
    {
        let bin = std::env::var("NIXPLANE").unwrap_or_else(|_| "nixplane".to_owned());
        let state_dir = std::env::var("NIXPLANE_STATE_DIR").ok();
        let transport =
            crate::commands::nixplane_transport::NixplaneCliTransport { bin, state_dir };
        let journal = match remote.last_sequence {
            None | Some(0) => transport.replay_journal(&attempt_id)?,
            Some(after) => transport.replay_from(&attempt_id, after)?,
        };
        nxr_task::observe_replay(run_dir, &remote.idempotency_key, &journal)?;
        let stored = nxr_task::remote_attempt_for_run(run_dir, &run_id)?.unwrap_or(remote);
        crate::commands::nixplane_transport::record_fetched_receipt(
            &transport,
            run_dir,
            &stored,
            output_name,
            &journal,
        )?;
    }
    let stored = nxr_task::remote_attempt_for_run(run_dir, &run_id)?
        .ok_or_else(|| "remote attempt is missing".to_owned())?;
    let mut value = json!({
        "schema": "remote-execution-receipt-v1",
        "run_id": stored.run_id,
        "operation_id": stored.operation_id,
        "source_set_id": stored.source_set_id,
        "attempt_id": stored.attempt_id,
        "lease_generation": stored.lease_generation,
        "last_sequence": stored.last_sequence,
        "phase": stored.phase,
    });
    if let Some(receipt) = nxr_task::read_remote_receipt(run_dir, &run_id)?
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "remote_receipt".to_owned(),
            serde_json::to_value(receipt).map_err(|err| err.to_string())?,
        );
    }
    Ok(value)
}

fn unverified_context() -> nxr_task::ContextIdentity {
    nxr_task::ContextIdentity {
        rustc_version: String::new(),
        wrapper_name: String::new(),
        target_dir: String::new(),
        cache_mode: String::new(),
    }
}

fn run_diagnostics(arguments: &Value, supervisor_dir: &Path) -> Result<Value, String> {
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.is_empty())
        .ok_or_else(|| "run_id is required".to_owned())?;
    let _ = nxr_core::execution_owner::inspect(supervisor_dir, run_id)?;
    let log = nxr_core::execution_owner::logs(supervisor_dir, run_id)?;
    let text = log.get("text").and_then(Value::as_str).unwrap_or("");
    let diagnostics = parse_run_log(text);
    let items = diagnostics
        .iter()
        .map(|item| {
            json!({
                "category": item.category,
                "file": item.file,
                "line": item.line,
                "excerpt": item.excerpt,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "run_id": run_id,
        "diagnostics": items
    }))
}

fn policy_view(
    records: &[OperationRecord],
    root: &Path,
    trusted: bool,
) -> Result<Vec<OperationRecord>, String> {
    if !trusted {
        return Ok(records.to_vec());
    }
    let gate = policy_gate(root)?;
    Ok(records
        .iter()
        .map(|record| apply_project_policy(record, &gate.applied))
        .collect())
}

struct PolicyGate {
    applied: ProjectExecutionPolicy,
    file: ProjectExecutionPolicy,
    approved: bool,
}

fn policy_gate(root: &Path) -> Result<PolicyGate, String> {
    let file = ProjectExecutionPolicy::load(root)?;
    if file.is_empty() {
        return Ok(PolicyGate {
            applied: file.clone(),
            file,
            approved: true,
        });
    }
    let approved = approved_policy_digest(root).as_deref() == Some(file.digest().as_str());
    let applied = if approved {
        file.clone()
    } else {
        ProjectExecutionPolicy::default()
    };
    Ok(PolicyGate {
        applied,
        file,
        approved,
    })
}

fn approved_policy_digest(root: &Path) -> Option<String> {
    let key = nxr_core::canonical_project_key(root).ok()?;
    nxr_core::TrustDatabase::load().ok()?.policy_digest(&key)
}

fn canonical_checkout(root: &Path) -> Result<PathBuf, String> {
    root.canonicalize()
        .map_err(|err| format!("checkout cannot be canonicalized: {err}"))
}

fn admission_id(workspace_id: &str, checkout: &Path, key: &str) -> Result<String, String> {
    let canonical = canonical_checkout(checkout)?;
    Ok(nxr_core::digest_bytes(
        format!("{workspace_id}\n{}\n{key}", canonical.display()).as_bytes(),
    ))
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

/// Cargo and Nix commands have an output contract. Other commands do not lock
/// the supervisor's `CARGO_TARGET_DIR`.
fn admitted_output_resources(
    workspace_root: &Path,
    command: &[String],
) -> Result<Vec<nxr_core::output_resource::OutputResource>, String> {
    if !command_has_output_contract(command) {
        return Ok(Vec::new());
    }
    let cargo_target = std::env::var("CARGO_TARGET_DIR").ok();
    nxr_core::output_resource::resolve_output_resources(
        workspace_root,
        command,
        cargo_target.as_deref(),
        None,
        std::env::consts::OS,
        std::env::consts::ARCH,
        "local",
    )
}

fn command_has_output_contract(command: &[String]) -> bool {
    command_locks_cargo_target(command) || command_is_nix(command)
}

fn command_is_nix(command: &[String]) -> bool {
    command.first().is_some_and(|arg| {
        matches!(
            Path::new(arg).file_name().and_then(|name| name.to_str()),
            Some("nix" | "nix-build")
        )
    })
}

fn command_is_heavy_cargo(command: &[String]) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("cargo")
    {
        return false;
    }
    command
        .iter()
        .skip(1)
        .any(|arg| arg == "build" || arg == "test" || arg == "nextest")
}

fn should_probe_child(command: &[String]) -> bool {
    if command_locks_cargo_target(command) {
        return true;
    }
    matches!(command.get(1).map(String::as_str), Some("task" | "run"))
}

fn canonical_parameters(parameters: &BTreeMap<String, String>) -> String {
    if parameters.is_empty() {
        return "none".to_owned();
    }
    parameters
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn execution_identity(
    operation_id: &str,
    parameters: &str,
    source_digest: &str,
    probe_script: Option<&str>,
    cwd: &Path,
) -> nxr_task::coalesce::ExecutionIdentity {
    let observation = probe_script.and_then(|script| run_probe(script, cwd));
    let toolchain = observation
        .as_ref()
        .map(|item| item.rustc_version.clone())
        .unwrap_or_default();
    let env_digest = nxr_core::digest_bytes(
        observation
            .as_ref()
            .map(|item| {
                format!(
                    "{}\n{}\n{}",
                    item.wrapper_path, item.target_directory, item.cache_identity
                )
            })
            .unwrap_or_default()
            .as_bytes(),
    );
    nxr_task::coalesce::ExecutionIdentity {
        operation_id: operation_id.to_owned(),
        parameters: parameters.to_owned(),
        source_digest: source_digest.to_owned(),
        toolchain,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        env_digest,
    }
}

fn run_probe(script: &str, cwd: &Path) -> Option<ChildObservation> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    parse_child_probe(&text).ok()
}

fn identity_verified(identity: &nxr_task::coalesce::ExecutionIdentity) -> bool {
    !identity.toolchain.is_empty()
}

/// A cancel from one of several observers only detaches that observer.
fn detach_observer(run_dir: &Path, run_id: &str, observer_id: &str) -> bool {
    matches!(
        nxr_task::detach_observer_id(run_dir, run_id, observer_id),
        Ok(nxr_task::DetachOutcome::Detach)
    )
}

fn observer_id_for_run(run_dir: &Path, run_id: &str, arguments: &Value) -> String {
    if let Some(key) = arguments
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
    {
        return key.to_owned();
    }
    nxr_task::idempotency_key_for_run(run_dir, run_id)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn reused_qualification(
    supervisor_dir: &Path,
    workspace_root: &Path,
    operation_id: &str,
    source_digest: &str,
    parameters: &str,
) -> Option<Value> {
    let path = supervisor_dir.join("qualifications").join(format!(
        "{}.json",
        nxr_core::digest_bytes(operation_id.as_bytes())
    ));
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let previous = qualification_record(&value)?;
    let toolchain = run_probe(child_probe_script(), workspace_root)
        .map(|observation| observation.rustc_version)
        .unwrap_or_default();
    let current = nxr_task::qualification::QualificationRecord {
        status: "passed".to_owned(),
        coverage: "complete".to_owned(),
        source_digest: source_digest.to_owned(),
        test_digest: source_digest.to_owned(),
        toolchain,
        parameters: parameters.to_owned(),
        tests_ran: true,
    };
    match nxr_task::qualification::reuse_passing_qualification(Some(&previous), &current) {
        nxr_task::qualification::ReuseDecision::Reuse => {
            let run_id = value
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if !run_id.is_empty() {
                let _ = nxr_core::execution_owner::note(
                    supervisor_dir,
                    &run_id,
                    "qualification reused",
                );
            }
            Some(json!({
                "run_id": run_id,
                "started": false,
                "replayed": true,
                "status": "reused"
            }))
        }
        _ => None,
    }
}

fn qualification_record(value: &Value) -> Option<nxr_task::qualification::QualificationRecord> {
    Some(nxr_task::qualification::QualificationRecord {
        status: value.get("status")?.as_str()?.to_owned(),
        coverage: value.get("coverage")?.as_str()?.to_owned(),
        source_digest: value.get("source_digest")?.as_str()?.to_owned(),
        test_digest: value.get("test_digest")?.as_str()?.to_owned(),
        toolchain: value.get("toolchain")?.as_str()?.to_owned(),
        parameters: value.get("parameters")?.as_str()?.to_owned(),
        tests_ran: value.get("tests_ran")?.as_bool()?,
    })
}

fn persist_child_observation(
    supervisor_dir: &Path,
    run_id: &str,
    operation_id: &str,
    workspace_id: &str,
    source_digest: &str,
    leaf_id: &str,
) {
    if source_digest.is_empty() {
        return;
    }
    let probe_path = supervisor_dir
        .join("provenance")
        .join(format!("{run_id}.{leaf_id}.probe"));
    let Ok(text) = std::fs::read_to_string(probe_path) else {
        return;
    };
    let Ok(observation) = parse_child_probe(&text) else {
        return;
    };
    let mut receipt = StoredReceipt {
        state: "planned".to_owned(),
        operation_id: operation_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        run_id: run_id.to_owned(),
        source_digest: source_digest.to_owned(),
        leaf_id: leaf_id.to_owned(),
        attempt: 1,
        observation: Some(observation),
    };
    for next in ["admitted", "started", "environment-verified"] {
        receipt = match transition(&receipt, next) {
            Ok(advanced) => advanced,
            Err(_) => return,
        };
    }
    let dir = supervisor_dir.join("provenance");
    let _ = write_receipt(&dir, &receipt);
}

fn stored_child_observation(supervisor_dir: &Path, run_id: &str) -> Option<ChildObservation> {
    let dir = supervisor_dir.join("provenance");
    let leaf = nxr_core::execution_owner::inspect(supervisor_dir, run_id)
        .ok()
        .and_then(|row| {
            row.get("leaf_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|leaf| !leaf.is_empty());
    let receipt = leaf
        .as_deref()
        .and_then(|leaf_id| read_leaf_receipt(&dir, run_id, leaf_id, 1).ok())
        .or_else(|| read_receipt(&dir, run_id, 1).ok())?;
    match receipt.state.as_str() {
        "environment-verified" | "completed" | "artifact-verified" | "qualified" => {
            receipt.observation
        }
        _ => None,
    }
}

fn shell_probe_from_observation(observation: &ChildObservation) -> ShellProbe {
    ShellProbe {
        rustc_path: observation.rustc_path.clone(),
        rustc_version: observation.rustc_version.clone(),
        cargo_path: observation.cargo_path.clone(),
        cargo_version: observation.cargo_version.clone(),
        wrapper_path: observation.wrapper_path.clone(),
        linker_path: observation.linker_path.clone(),
        target_directory: observation.target_directory.clone(),
        cache_identity: observation.cache_identity.clone(),
    }
}

/// A cargo argv, or an explicit `--target-dir`, owns one output directory.
/// `echo`, `sleep`, and `nxr task` do not lock the supervisor's `CARGO_TARGET_DIR`.
fn command_locks_cargo_target(command: &[String]) -> bool {
    if command
        .iter()
        .any(|arg| arg == "--target-dir" || arg.starts_with("--target-dir="))
    {
        return true;
    }
    command.first().is_some_and(|arg| {
        Path::new(arg).file_name().and_then(|name| name.to_str()) == Some("cargo")
    })
}

fn command_for(
    launch: &Launch,
    record: &OperationRecord,
    parameters: &[String],
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
            let mut command = vec![executable.display().to_string(), verb.to_owned()];
            if verb == "task" {
                let jobs = std::thread::available_parallelism()
                    .map(|count| count.get())
                    .unwrap_or(1)
                    .max(1);
                command.push("-j".to_owned());
                command.push(jobs.to_string());
            }
            command.push(record.name.clone());
            if !parameters.is_empty() {
                command.push("--".to_owned());
                command.extend(parameters.iter().cloned());
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
    if let Some(remote) = nxr_task::remote_attempt_for_run(run_dir, run_id)?
        && remote.attempt_id.is_some()
    {
        let bin = std::env::var("NIXPLANE").unwrap_or_else(|_| "nixplane".to_owned());
        let state_dir = std::env::var("NIXPLANE_STATE_DIR").ok();
        let transport =
            crate::commands::nixplane_transport::NixplaneCliTransport { bin, state_dir };
        return nxr_task::cancel_and_bind(run_dir, &remote.idempotency_key, &transport);
    }
    let owned = inspect(run_dir, run_id, workspace_id)?;
    if owned.is_none() {
        return Ok(json!({
            "cancellation_requested": false,
            "process_cancelled": false,
            "process_terminal": false,
            "reservation_cancelled": false
        }));
    }
    let observer = observer_id_for_run(run_dir, run_id, arguments);
    if detach_observer(run_dir, run_id, &observer) {
        return Ok(json!({
            "cancellation_requested": true,
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
        "operations_plan_batch" => &["paths"],
        "runs_inspect" | "runs_cancel" => &["run_id"],
        "runs_get_receipt" => &[
            "id",
            "run_id",
            "output_name",
            "source_set_id",
            "lease_generation",
        ],
        "runs_get_diagnostics" => &["run_id"],
        "workspace_affected" => &["paths"],
        "operations_start" => &["id", "idempotency_key", "parameters"],
        "operations_start_remote" => &[
            "id",
            "idempotency_key",
            "spec",
            "source_set",
            "dirty_manifest",
            "source_locator",
            "parameters",
        ],
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

fn plan_batch(
    arguments: &Value,
    records: &[OperationRecord],
    workspace_id: &str,
    workspace_root: &Path,
    flake: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
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
    let dirty = tree_is_dirty(workspace_root);
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
        true,
        &paths,
        runner,
    );
    let plan = match selection {
        Err(_) => validation_plan(&ValidationPlanInput {
            discovery_failed: true,
            dirty,
            changed_paths: &paths,
            unknown_nodes: &[],
            operations: &[],
            undeclared_members: &[],
        }),
        Ok(selection) => {
            let unknown: Vec<String> = selection
                .analysis
                .nodes
                .iter()
                .filter(|node| node.status == NodeStatus::Unknown)
                .map(|node| node.name.clone())
                .collect();
            let ids: std::collections::BTreeSet<String> = selection
                .analysis
                .tasks
                .iter()
                .map(|name| format!("{workspace_id}:task:{name}"))
                .collect();
            let operations: Vec<OperationRecord> = records
                .iter()
                .filter(|record| ids.contains(&record.operation_id))
                .cloned()
                .collect();
            validation_plan(&ValidationPlanInput {
                discovery_failed: false,
                dirty,
                changed_paths: &selection.analysis.changed_paths,
                unknown_nodes: &unknown,
                operations: &operations,
                undeclared_members: &[],
            })
        }
    };
    Ok(json!(plan))
}

fn tree_is_dirty(root: &Path) -> bool {
    let Some(root) = Utf8Path::from_path(root) else {
        return true;
    };
    git_working_tree_changes(root)
        .map(|paths| !paths.is_empty())
        .unwrap_or(true)
}

fn reservation_dir(workspace_id: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    let canonical = canonical_checkout(workspace_root)?;
    let material = format!("{workspace_id}\n{}", canonical.display());
    let digest = nxr_core::digest_bytes(material.as_bytes());
    Ok(nxr_core::state_dir()
        .join("mcp-reservations")
        .join(&digest[..32]))
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

    #[test]
    #[ignore = "disposable hub"]
    fn disposable_hub_receipt_binds_the_attempt() {
        let spec_path = std::env::var("C24_SPEC").expect("C24_SPEC");
        let source_path = std::env::var("C24_SOURCE").expect("C24_SOURCE");
        let locator_path = std::env::var("C24_LOCATOR").expect("C24_LOCATOR");
        let state_dir = std::env::var("NIXPLANE_STATE_DIR").expect("NIXPLANE_STATE_DIR");
        let data_dir = std::env::var("NIXPLANE_DATA_DIR").expect("NIXPLANE_DATA_DIR");
        let token_file =
            std::env::var("NIXPLANE_WORKER_TOKEN_FILE").expect("NIXPLANE_WORKER_TOKEN_FILE");
        let system = std::env::var("C24_SYSTEM").expect("C24_SYSTEM");
        let nixplane = std::env::var("NIXPLANE").expect("NIXPLANE");
        let hub = std::env::var("NIXPLANE_HUB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:19580".to_owned());
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(&spec_path).expect("spec"))
            .expect("spec json");
        let source_set: Value =
            serde_json::from_str(&std::fs::read_to_string(&source_path).expect("source"))
                .expect("source json");
        let locator = std::fs::read_to_string(&locator_path).expect("locator");
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".nxr")).expect("policy dir");
        std::fs::write(
            root.path().join(".nxr/execution-policy.toml"),
            "[[remote_operation]]\nid = \"lattice:task:c24-marker\"\n",
        )
        .expect("policy");
        let trust = root.path().join("trust.json");
        nxr_core::set_trust_db_override(Some(trust));
        let key = nxr_core::canonical_project_key(root.path()).expect("project key");
        let mut database = nxr_core::TrustDatabase::load().expect("trust");
        database.add_trust(&key).expect("trust project");
        let policy = nxr_task::ProjectExecutionPolicy::load(root.path()).expect("policy load");
        database
            .approve_policy(&key, &policy.digest())
            .expect("approve");
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: root.path().display().to_string(),
            source_revision: None,
        };
        let record = operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "c24-marker", "flake.nix"),
        );
        let other = operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "other", "flake.nix"),
        );
        let attempts_before = hub_attempt_count(&data_dir);
        let denied = start_remote(
            &json!({
                "id": other.operation_id,
                "idempotency_key": "c24-denied",
                "spec": spec.clone(),
                "source_set": source_set.clone(),
                "source_locator": locator.trim(),
            }),
            &[other.clone(), record.clone()],
            "lattice",
            root.path(),
            root.path(),
            false,
        );
        assert_eq!(
            denied.expect_err("denied"),
            "remote execution is not authorized"
        );
        assert_eq!(hub_attempt_count(&data_dir), attempts_before);
        let started = start_remote(
            &json!({
                "id": record.operation_id,
                "idempotency_key": "c24-nxr-1",
                "spec": spec.clone(),
                "source_set": source_set.clone(),
                "source_locator": locator.trim(),
            }),
            std::slice::from_ref(&record),
            "lattice",
            root.path(),
            root.path(),
            false,
        )
        .expect("submit");
        let again = start_remote(
            &json!({
                "id": record.operation_id,
                "idempotency_key": "c24-nxr-1",
                "spec": spec.clone(),
                "source_set": source_set.clone(),
                "source_locator": locator.trim(),
            }),
            std::slice::from_ref(&record),
            "lattice",
            root.path(),
            root.path(),
            false,
        )
        .expect("replay");
        assert_eq!(again["attempt_id"], started["attempt_id"]);
        assert_eq!(again["replayed"], true);
        let token = std::fs::read_to_string(&token_file).expect("token");
        let worker = std::process::Command::new(&nixplane)
            .args([
                "--json",
                "nxb",
                "worker",
                "--hub",
                &hub,
                "--facet",
                "c24-host/linux",
                "--physical-host",
                "c24-host",
                "--system",
                &system,
                "--max-jobs",
                "1",
                "--cores",
                "1",
                "--memory-mib",
                "1024",
                "--feature",
                "native_command",
                "--once",
            ])
            .env("NIXPLANE_STATE_DIR", &state_dir)
            .env("NIXPLANE_WORKER_TOKEN", token.trim())
            .env("NIXPLANE_HUB_URL", &hub)
            .output()
            .expect("worker");
        assert!(
            worker.status.success(),
            "worker {}",
            String::from_utf8_lossy(&worker.stderr)
        );
        let run_id = started["run_id"].as_str().expect("run");
        let receipt = run_receipt(
            &json!({
                "id": record.operation_id,
                "run_id": run_id,
                "output_name": "out.txt",
            }),
            std::slice::from_ref(&record),
            "lattice",
            root.path(),
            root.path(),
            root.path(),
            false,
        )
        .expect("receipt");
        let digest = receipt["remote_receipt"]["output_digest"]
            .as_str()
            .expect("digest");
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        assert!(hex.len() == 64, "{receipt}");
        assert_eq!(receipt["source_set_id"], spec["source_set_id"]);
        let wrong = run_receipt(
            &json!({
                "id": record.operation_id,
                "run_id": run_id,
                "lease_generation": 99,
            }),
            std::slice::from_ref(&record),
            "lattice",
            root.path(),
            root.path(),
            root.path(),
            false,
        );
        assert_eq!(
            wrong.expect_err("lease"),
            "lease does not match the attempt"
        );
        let queued = start_remote(
            &json!({
                "id": record.operation_id,
                "idempotency_key": "c24-nxr-cancel",
                "spec": spec.clone(),
                "source_set": source_set.clone(),
                "source_locator": locator.trim(),
            }),
            std::slice::from_ref(&record),
            "lattice",
            root.path(),
            root.path(),
            false,
        )
        .expect("cancel submit");
        let cancelled = cancel_run(
            root.path(),
            root.path(),
            root.path(),
            "lattice",
            &json!({"run_id": queued["run_id"].as_str().expect("cancel run")}),
        )
        .expect("cancel");
        assert_eq!(cancelled["cancel_complete"], true);
        assert!(cancelled.get("process_terminal").is_none());
        let text = serde_json::to_string_pretty(&receipt).expect("json");
        let path =
            std::env::var("C24_RECEIPT").unwrap_or_else(|_| "/tmp/c24-nxr-receipt.json".to_owned());
        std::fs::write(path, text).expect("write receipt");
    }

    fn hub_attempt_count(data_dir: &str) -> i64 {
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg("import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('select count(*) from execution_attempts').fetchone()[0])")
            .arg(format!("{data_dir}/hub.sqlite"))
            .output()
            .expect("count");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("count parse")
    }

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
    fn receipt_copies_supervisor_events_and_leaves_the_shell_unentered() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let supervisor = dir.path().join("supervisor");
        let true_bin = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let started = nxr_core::execution_owner::start(
            &supervisor,
            &json!({
                "command": [true_bin],
                "label": "receipt-fixture",
                "operation_id": "lattice:task:test",
                "workspace_id": "lattice"
            }),
        )
        .unwrap();
        let run_id = started["id"].as_str().unwrap();
        for _ in 0..50 {
            let record = nxr_core::execution_owner::inspect(&supervisor, run_id).unwrap();
            if record["status"] == "exited" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let receipt = invoke(
            &json!({
                "name": "runs_get_receipt",
                "arguments": {"id": "lattice:task:test", "run_id": run_id}
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            &dir.path().join("nxrd.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(receipt["shell_entered"], false);
        assert_eq!(receipt["rustc"]["status"], "not entered");
        assert_eq!(receipt["schema"], "execution-receipt-v1");
        assert_eq!(receipt["cache_reason"], "context_unverified");
        let stored = nxr_core::execution_owner::events(&supervisor, run_id, 0).unwrap();
        assert_eq!(receipt["events"], stored);
        assert!(
            stored
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["message"] == "started")
        );
        forget_run(run_id);
    }

    #[test]
    fn receipt_rejects_a_run_for_another_operation() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let supervisor = dir.path().join("supervisor");
        let true_bin = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let started = nxr_core::execution_owner::start(
            &supervisor,
            &json!({
                "command": [true_bin],
                "label": "other-operation",
                "operation_id": "lattice:task:reset-db",
                "workspace_id": "lattice"
            }),
        )
        .unwrap();
        let run_id = started["id"].as_str().unwrap();
        let receipt = invoke(
            &json!({
                "name": "runs_get_receipt",
                "arguments": {"id": "lattice:task:test", "run_id": run_id}
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            &dir.path().join("nxrd.sock"),
            true,
            &Launch::Task,
        );
        assert!(receipt.unwrap_err().contains("does not match"));
        forget_run(run_id);
    }

    #[test]
    fn receipt_uses_the_child_observation_stored_for_that_run() {
        let records = records();
        let dir = tempfile::tempdir().unwrap();
        let supervisor = dir.path().join("supervisor");
        std::fs::create_dir_all(&supervisor).unwrap();
        let true_bin = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let started = nxr_core::execution_owner::start(
            &supervisor,
            &json!({
                "command": [true_bin],
                "label": "observed",
                "operation_id": "lattice:task:test",
                "workspace_id": "lattice"
            }),
        )
        .unwrap();
        let run_id = started["id"].as_str().unwrap();
        let observation = nxr_task::ChildObservation {
            rustc_path: "/nix/store/rustc".into(),
            rustc_version: "rustc 1.80.0".into(),
            cargo_path: "/nix/store/cargo".into(),
            cargo_version: "cargo 1.80.0".into(),
            wrapper_path: String::new(),
            linker_path: String::new(),
            target_directory: "/tmp/nxr-q-target".into(),
            cache_identity: String::new(),
        };
        let mut stored = nxr_task::StoredReceipt {
            state: "planned".into(),
            operation_id: "lattice:task:test".into(),
            workspace_id: "lattice".into(),
            run_id: run_id.into(),
            source_digest: "src".into(),
            leaf_id: String::new(),
            attempt: 1,
            observation: Some(observation),
        };
        for next in ["admitted", "started", "environment-verified"] {
            stored = nxr_task::transition(&stored, next).unwrap();
        }
        let provenance = supervisor.join("provenance");
        nxr_task::write_receipt(&provenance, &stored).unwrap();
        let receipt = invoke(
            &json!({
                "name": "runs_get_receipt",
                "arguments": {"id": "lattice:task:test", "run_id": run_id}
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            &dir.path().join("nxrd.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(receipt["shell_entered"], true);
        assert_eq!(receipt["rustc"]["status"], "entered");
        assert_eq!(receipt["rustc"]["version"], "rustc 1.80.0");
        assert_eq!(receipt["wrapper"]["status"], "not entered");
        assert_ne!(receipt["cache_reason"], "context_unverified");
        let wrong = invoke(
            &json!({
                "name": "runs_get_receipt",
                "arguments": {"id": "lattice:task:reset-db", "run_id": run_id}
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            &dir.path().join("nxrd.sock"),
            true,
            &Launch::Task,
        );
        assert!(wrong.unwrap_err().contains("does not match"));
        forget_run(run_id);
    }

    #[test]
    fn diagnostics_classify_the_existing_log() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = dir.path().join("supervisor");
        let started = nxr_core::execution_owner::start(
            &supervisor,
            &json!({
                "command": ["/bin/sh", "-c", "echo policy approval required"],
                "label": "diagnostic-fixture",
                "operation_id": "lattice:task:test",
                "workspace_id": "lattice"
            }),
        )
        .unwrap();
        let run_id = started["id"].as_str().unwrap();
        for _ in 0..50 {
            let record = nxr_core::execution_owner::inspect(&supervisor, run_id).unwrap();
            if record["status"] == "exited" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let report = invoke(
            &json!({
                "name": "runs_get_diagnostics",
                "arguments": {"run_id": run_id}
            }),
            &records(),
            std::path::Path::new("/tmp/lattice"),
            dir.path(),
            &dir.path().join("nxrd.sock"),
            true,
            &Launch::Task,
        )
        .unwrap();
        assert_eq!(report["run_id"], run_id);
        assert_eq!(report["diagnostics"][0]["category"], "policy_refusal");
        assert!(report["diagnostics"][0]["file"].is_null());
        assert!(report["diagnostics"][0]["line"].is_null());
        forget_run(run_id);
    }

    #[test]
    fn plan_batch_rejects_a_client_authorization_flag() {
        let records = records();
        let refused = invoke(
            &json!({
                "name": "operations_plan_batch",
                "arguments": {"paths": ["crates/nxr-cli"], "authorized": true}
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            std::path::Path::new("/tmp/unused"),
            std::path::Path::new("/tmp/missing.sock"),
            true,
            &Launch::Task,
        );
        assert!(refused.unwrap_err().contains("unknown argument"));
        let start = invoke(
            &json!({
                "name": "operations_start",
                "arguments": {
                    "id": "lattice:task:test",
                    "idempotency_key": "budget",
                    "caller_role": "parent",
                    "cpu": 99
                }
            }),
            &records,
            std::path::Path::new("/tmp/lattice"),
            std::path::Path::new("/tmp/unused"),
            std::path::Path::new("/tmp/missing.sock"),
            true,
            &Launch::Task,
        );
        assert!(start.unwrap_err().contains("unknown argument"));
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
        let (command, _) = command_for(&Launch::Task, &records[0], &[], root.path()).unwrap();
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

    fn approve_policy_file(root: &std::path::Path) -> std::path::PathBuf {
        let database = root.join("trust.json");
        nxr_core::set_trust_db_override(Some(database.clone()));
        let key = nxr_core::canonical_project_key(root).unwrap();
        let mut stored = nxr_core::TrustDatabase::load().unwrap();
        stored.add_trust(&key).unwrap();
        let policy = nxr_task::ProjectExecutionPolicy::load(root).unwrap();
        stored.approve_policy(&key, &policy.digest()).unwrap();
        database
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
            operation_record(
                &workspace,
                &Declaration::named(OperationKind::Task, "cli-build", "flake.nix"),
            ),
        ];
        let _database = approve_policy_file(root.path());
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
        std::fs::write(
            root.path().join(".nxr/execution-policy.toml"),
            "[[operation]]\nid = \"lattice:task:rust-test-pkgs\"\nparameters = [\"package\"]\n\n[[operation]]\nid = \"lattice:task:cli-build\"\n",
        )
        .unwrap();
        let widened = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:cli-build", "idempotency_key": "build"}}),
            &records,
            root.path(),
            root.path(),
            &root.path().join("missing.sock"),
            true,
            &Launch::Task,
        );
        assert!(widened.unwrap_err().contains("policy approval required"));
        nxr_core::set_trust_db_override(None);
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_and_its_real_path_share_one_run() {
        let real = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("checkout");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        assert_eq!(
            reservation_dir("lattice", real.path()).unwrap(),
            reservation_dir("lattice", &link).unwrap()
        );
        let daemon = tempfile::tempdir().unwrap();
        let socket = boot(daemon.path());
        let reservations = reservation_dir("lattice", real.path()).unwrap();
        std::fs::create_dir_all(&reservations).unwrap();
        let workspace = WorkspaceInstance {
            workspace_id: "lattice".to_owned(),
            workspace_root: real.path().display().to_string(),
            source_revision: None,
        };
        let records = vec![operation_record(
            &workspace,
            &Declaration::named(OperationKind::Task, "test", "flake.nix"),
        )];
        let launch = Launch::Command(vec!["/bin/echo".into(), "ok".into()]);
        let started = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "same"}}),
            &records,
            real.path(),
            &reservations,
            &socket,
            true,
            &launch,
        )
        .unwrap();
        let again = invoke(
            &json!({"name": "operations_start", "arguments": {"id": "lattice:task:test", "idempotency_key": "same"}}),
            &records,
            &link,
            &reservations,
            &socket,
            true,
            &launch,
        )
        .unwrap();
        assert_eq!(started["run_id"], again["run_id"]);
        assert_eq!(again["started"], false);
        assert_eq!(list_on(&socket).unwrap().as_array().unwrap().len(), 1);
        forget_run(started["run_id"].as_str().unwrap());
    }
}
