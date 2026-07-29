//! `nxr up` / `status` / `logs` / `down` for long-running process nodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_core::{DaemonClientError, daemon_socket_path, log_broker_enabled, try_connect};
use nxr_nix::{OptionalNixFlags, resolve_app_by_name};
use nxr_task::{
    ProcessDefinition, ProcessNameError, ProcessReadiness, SchemaError, apply_task_context,
    dependency_base_name, resolve_env_provider_secrets_with, sanitize_process_log_name,
    validate_node_id,
};
use serde::{Deserialize, Serialize};

use crate::commands::common::{
    AppRequest, PrepareError, WorkspaceSnapshot, prepare_fast_app_plan,
    resolve_task_execution_directory,
};
use crate::flake::FlakeResolveError;
use crate::runner_output::RunnerOutput;
use crate::shell_mode::ShellMode;

const PROCESS_STATE_SCHEMA_VERSION: u32 = 1;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Errors while supervising process nodes.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("process not found: {name}")]
    NotFound { name: String },
    #[error("process `{name}` is not running")]
    NotRunning { name: String },
    #[error("process `{name}` is already running (pid {pid})")]
    AlreadyRunning { name: String, pid: u32 },
    #[error("no process definitions found for this flake")]
    NoProcesses,
    #[error("failed to supervise process `{name}`: {message}")]
    Supervision { name: String, message: String },
    #[error("invalid process name `{name}`: {message}")]
    InvalidName { name: String, message: String },
    #[error("process `{name}` pid {pid} no longer matches supervised identity")]
    IdentityMismatch { name: String, pid: u32 },
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Context(#[from] nxr_task::ContextError),
}

impl ProcessError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::NotFound { .. } | Self::NotRunning { .. } => exit::NOT_FOUND,
            Self::AlreadyRunning { .. } => exit::USAGE,
            Self::NoProcesses => exit::NOT_FOUND,
            Self::InvalidName { .. } => exit::USAGE,
            Self::IdentityMismatch { .. } => exit::PROCESS_SUPERVISION,
            Self::Supervision { .. } => exit::PROCESS_SUPERVISION,
            Self::Schema(_) | Self::Context(_) => exit::USAGE,
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ProcessStateFile {
    schema_version: u32,
    project_id: String,
    processes: BTreeMap<String, RunningProcessRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct RunningProcessRecord {
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_time: Option<u64>,
    app: String,
    log_path: String,
    started_at: String,
    ready: bool,
}

/// Start one or all process nodes.
pub fn up(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    names: &[String],
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<i32, ProcessError> {
    let context = discover_process_context(flake_arg, nix_override, nix_flags)?;
    context.document.validate()?;
    let targets = resolve_targets(&context.document.processes, names)?;
    let mut state = load_state(&context.project_id)?;

    for name in targets {
        if let Some(record) = state.processes.get(&name)
            && is_supervised_process_alive(record)
        {
            return Err(ProcessError::AlreadyRunning {
                name,
                pid: record.pid,
            });
        }
        let definition = context
            .document
            .processes
            .get(&name)
            .expect("resolved process");
        if !dependencies_ready(definition, &state.processes) {
            return Err(ProcessError::Supervision {
                name: name.clone(),
                message: "dependency not ready".to_owned(),
            });
        }
        runner
            .info(format!("starting process {name}"))
            .map_err(ProcessError::Io)?;
        let record = spawn_process(&context, &name, definition)?;
        state.processes.insert(name.clone(), record);
        save_state(&context.project_id, &state)?;
    }

    Ok(exit::SUCCESS)
}

/// Show supervised process status.
pub fn status(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<i32, ProcessError> {
    let context = discover_process_context(flake_arg, nix_override, nix_flags)?;
    let state = load_state(&context.project_id)?;
    runner
        .info("checking process status")
        .map_err(ProcessError::Io)?;

    let mut stdout = io::stdout().lock();
    if json {
        let payload = serde_json::json!({
            "schema_version": 1,
            "flake": context.flake.display,
            "processes": context.document.processes.keys().collect::<Vec<_>>(),
            "running": state.processes,
        });
        writeln!(stdout, "{}", serde_json::to_string_pretty(&payload)?)?;
        return Ok(exit::SUCCESS);
    }

    if context.document.processes.is_empty() {
        writeln!(
            stdout,
            "No process definitions for {}",
            context.flake.display
        )?;
        return Ok(exit::SUCCESS);
    }

    writeln!(stdout, "Processes for {}:", context.flake.display)?;
    for name in context.document.processes.keys() {
        if let Some(record) = state.processes.get(name) {
            let alive = is_supervised_process_alive(record);
            let ready = alive && record.ready;
            writeln!(
                stdout,
                "  {name}: running (pid {}, ready={ready}, log={})",
                record.pid, record.log_path
            )?;
        } else {
            writeln!(stdout, "  {name}: stopped")?;
        }
    }
    Ok(exit::SUCCESS)
}

/// Tail logs for a named process.
pub fn logs(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    name: &str,
    follow: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<i32, ProcessError> {
    let context = discover_process_context(flake_arg, nix_override, nix_flags)?;
    if !context.document.processes.contains_key(name) {
        return Err(ProcessError::NotFound {
            name: name.to_owned(),
        });
    }
    let state = load_state(&context.project_id)?;
    let record = state
        .processes
        .get(name)
        .ok_or_else(|| ProcessError::NotRunning {
            name: name.to_owned(),
        })?;
    runner
        .info(format!("reading logs for {name}"))
        .map_err(ProcessError::Io)?;

    if follow && log_broker_enabled() && follow_logs_via_broker(&context.project_id, name, record)?
    {
        return Ok(exit::SUCCESS);
    }

    let mut file = File::open(&record.log_path)?;
    let mut stdout = io::stdout().lock();
    if follow {
        let mut buffer = [0u8; 4096];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                if !is_supervised_process_alive(record) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            stdout.write_all(&buffer[..read])?;
        }
    } else {
        io::copy(&mut file, &mut stdout)?;
    }
    Ok(exit::SUCCESS)
}

/// Prefer nxrd log.subscribe when the daemon is up; return false to fall back.
#[cfg(unix)]
fn follow_logs_via_broker(
    project_id: &str,
    name: &str,
    record: &RunningProcessRecord,
) -> Result<bool, ProcessError> {
    let socket = daemon_socket_path();
    let mut conn = match try_connect(&socket) {
        Ok(conn) => conn,
        Err(
            DaemonClientError::Absent
            | DaemonClientError::Disabled
            | DaemonClientError::ProtocolMismatch(_),
        ) => return Ok(false),
        Err(error) => {
            runner_broker_fallback_note(&error);
            return Ok(false);
        }
    };

    let stream_id = log_stream_id(project_id, name);
    let alive_record = record.clone();
    let mut stdout = io::stdout().lock();
    let mut wrote_any = false;

    match conn.follow_log_stream(
        &stream_id,
        Some(alive_record.log_path.as_str()),
        true,
        |chunk| {
            wrote_any = true;
            stdout.write_all(chunk).map_err(DaemonClientError::Io)?;
            stdout.flush().map_err(DaemonClientError::Io)?;
            Ok(())
        },
        || is_supervised_process_alive(&alive_record),
    ) {
        Ok(()) => Ok(true),
        Err(
            DaemonClientError::Absent
            | DaemonClientError::Disabled
            | DaemonClientError::ProtocolMismatch(_),
        ) => Ok(false),
        Err(error) if wrote_any => {
            // Partial stream already emitted; avoid file-follow replay.
            runner_broker_fallback_note(&error);
            Ok(true)
        }
        Err(error) => {
            runner_broker_fallback_note(&error);
            Ok(false)
        }
    }
}

#[cfg(not(unix))]
fn follow_logs_via_broker(
    _project_id: &str,
    _name: &str,
    _record: &RunningProcessRecord,
) -> Result<bool, ProcessError> {
    Ok(false)
}

fn runner_broker_fallback_note(error: &DaemonClientError) {
    // Best-effort diagnostics only; never fail follow because stderr is busy.
    let _ = writeln!(
        io::stderr(),
        "nxr: log broker unavailable ({error}); falling back to file follow"
    );
}

fn log_stream_id(project_id: &str, name: &str) -> String {
    format!("{project_id}/{name}")
}

/// Best-effort registration so the daemon can track the log path early.
fn best_effort_log_open(project_id: &str, name: &str, log_path: &Path) {
    if !log_broker_enabled() {
        return;
    }
    let socket = daemon_socket_path();
    let Ok(mut conn) = try_connect(&socket) else {
        return;
    };
    let stream = log_stream_id(project_id, name);
    let _ = conn.call::<serde_json::Value>(
        "log.open",
        Some(serde_json::json!({
            "stream": stream,
            "path": log_path.display().to_string(),
        })),
    );
}

/// Stop one or all supervised processes.
pub fn down(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    names: &[String],
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<i32, ProcessError> {
    let context = discover_process_context(flake_arg, nix_override, nix_flags)?;
    let mut state = load_state(&context.project_id)?;
    let targets: Vec<String> = if names.is_empty() {
        state.processes.keys().cloned().collect()
    } else {
        names.to_vec()
    };

    for name in targets {
        let Some(record) = state.processes.remove(&name) else {
            if names.is_empty() {
                continue;
            }
            return Err(ProcessError::NotRunning { name });
        };
        runner
            .info(format!("stopping process {name}"))
            .map_err(ProcessError::Io)?;
        if !is_supervised_process_alive(&record) {
            continue;
        }
        terminate_supervised_process(&name, &record)?;
    }

    save_state(&context.project_id, &state)?;
    Ok(exit::SUCCESS)
}

struct ProcessContext<'a> {
    flake: crate::flake::FlakeSelection,
    nix_override: Option<&'a str>,
    nix_flags: &'a OptionalNixFlags,
    apps: Vec<nxr_core::App>,
    document: nxr_task::TaskDocument,
    project_id: String,
}

fn discover_process_context<'a>(
    flake_arg: Option<&'a str>,
    nix_override: Option<&'a str>,
    nix_flags: &'a OptionalNixFlags,
) -> Result<ProcessContext<'a>, ProcessError> {
    let snapshot = WorkspaceSnapshot::load(flake_arg, nix_override, true, nix_flags)?;
    let document = snapshot
        .tasks
        .as_ref()
        .cloned()
        .unwrap_or_else(|| nxr_task::TaskDocument::new(BTreeMap::new()));
    if document.processes.is_empty() {
        return Err(ProcessError::NoProcesses);
    }
    let project_id = project_identity(
        snapshot.flake.local_root.as_deref(),
        &snapshot.flake.nix_ref,
    );
    Ok(ProcessContext {
        flake: snapshot.flake,
        nix_override,
        nix_flags,
        apps: snapshot.apps.values().cloned().collect(),
        document,
        project_id,
    })
}

fn resolve_targets(
    processes: &BTreeMap<String, ProcessDefinition>,
    names: &[String],
) -> Result<Vec<String>, ProcessError> {
    let seeds: Vec<String> = if names.is_empty() {
        processes.keys().cloned().collect()
    } else {
        for name in names {
            validate_node_id(name).map_err(|error| ProcessError::InvalidName {
                name: name.clone(),
                message: process_name_error_message(&error),
            })?;
            if !processes.contains_key(name) {
                return Err(ProcessError::NotFound { name: name.clone() });
            }
        }
        names.to_vec()
    };

    let mut needed = BTreeSet::new();
    let mut stack = seeds;
    while let Some(name) = stack.pop() {
        if !needed.insert(name.clone()) {
            continue;
        }
        let definition = processes
            .get(&name)
            .ok_or_else(|| ProcessError::NotFound { name: name.clone() })?;
        for dependency in &definition.depends_on {
            let base = dependency_base_name(dependency).to_owned();
            if !processes.contains_key(&base) {
                return Err(ProcessError::Supervision {
                    name: name.clone(),
                    message: format!("unknown dependency `{dependency}`"),
                });
            }
            stack.push(base);
        }
    }

    // Kahn topo among the closed set.
    let mut indegree: BTreeMap<String, usize> = needed.iter().map(|n| (n.clone(), 0)).collect();
    let mut adjacency: BTreeMap<String, Vec<String>> =
        needed.iter().map(|n| (n.clone(), Vec::new())).collect();
    for name in &needed {
        let definition = processes.get(name).expect("needed process");
        for dependency in &definition.depends_on {
            let base = dependency_base_name(dependency).to_owned();
            if !needed.contains(&base) {
                continue;
            }
            adjacency.get_mut(&base).expect("adj").push(name.clone());
            *indegree.get_mut(name).expect("indegree") += 1;
        }
    }

    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|&(_, degree)| *degree == 0)
        .map(|(name, _)| name.clone())
        .collect();
    let mut ordered = Vec::with_capacity(needed.len());
    while let Some(name) = ready.iter().next().cloned() {
        ready.remove(&name);
        ordered.push(name.clone());
        let next = adjacency.remove(&name).unwrap_or_default();
        for child in next {
            let degree = indegree.get_mut(&child).expect("indegree");
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.insert(child);
            }
        }
    }

    if ordered.len() != needed.len() {
        return Err(ProcessError::Supervision {
            name: "processes".to_owned(),
            message: "dependency cycle detected".to_owned(),
        });
    }
    Ok(ordered)
}

fn dependencies_ready(
    definition: &ProcessDefinition,
    running: &BTreeMap<String, RunningProcessRecord>,
) -> bool {
    definition.depends_on.iter().all(|dependency| {
        let base = dependency_base_name(dependency);
        running
            .get(base)
            .is_some_and(|record| record.ready || !dependency.contains('@'))
    })
}

fn spawn_process(
    context: &ProcessContext<'_>,
    name: &str,
    definition: &ProcessDefinition,
) -> Result<RunningProcessRecord, ProcessError> {
    let _app = resolve_app_by_name(&context.apps, &definition.app).map_err(|error| {
        ProcessError::Supervision {
            name: name.to_owned(),
            message: error.to_string(),
        }
    })?;

    let cli_policy = EnvironmentPolicy::Inherit;
    let (environment_policy, spawn_overrides, shell_name) =
        if let Some(context_name) = definition.context.as_deref() {
            let applied = apply_task_context(
                &context.document,
                &format!("process:{name}"),
                context_name,
                &cli_policy,
            )?;
            if applied.confirm {
                return Err(ProcessError::Supervision {
                    name: name.to_owned(),
                    message: "process contexts with confirm=true are not supported for `nxr up`"
                        .to_owned(),
                });
            }
            let mut overrides = applied.spawn_env_set;
            let secrets = resolve_env_provider_secrets_with(&applied.plan_secrets, |reference| {
                std::env::var(reference).ok()
            })?;
            overrides.extend(secrets);
            let shell = definition.shell.clone().or(applied.shell);
            (applied.environment_policy, overrides, shell)
        } else {
            (cli_policy, BTreeMap::new(), definition.shell.clone())
        };

    let app_request = process_app_request(
        context,
        definition.app.as_str(),
        &definition.arguments,
        shell_name.as_deref(),
        environment_policy.clone(),
    );
    let mut prepared = prepare_fast_app_plan(&app_request)?;
    if definition.working_directory.is_some() {
        let invocation_cwd = crate::commands::common::current_invocation_directory()?;
        prepared.execution_directory = resolve_task_execution_directory(
            &invocation_cwd,
            &context.flake,
            false,
            None,
            definition.working_directory.as_deref(),
        )?;
    }

    let spawn = crate::commands::store_exe::resolve_app_spawn(
        &prepared.plan,
        &prepared.nix,
        prepared.local_root.as_deref(),
        context.nix_flags,
        "",
        Some(prepared.execution_directory.as_std_path()),
    );

    let log_path = process_log_path(&context.project_id, name)?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    best_effort_log_open(context.project_id.as_str(), name, &log_path);

    let pid = spawn_background(
        spawn.program.as_std_path(),
        &spawn.arguments,
        prepared.execution_directory.as_std_path(),
        &environment_policy,
        &spawn_overrides,
        log_file,
    )
    .map_err(|error| ProcessError::Supervision {
        name: name.to_owned(),
        message: error.to_string(),
    })?;

    let start_time = capture_pid_start_time(pid);
    let ready = match wait_for_readiness(definition.readiness.as_ref(), pid) {
        Ok(ready) => ready,
        Err(message) => {
            let _ = terminate_pid(pid);
            return Err(ProcessError::Supervision {
                name: name.to_owned(),
                message,
            });
        }
    };
    Ok(RunningProcessRecord {
        pid,
        start_time,
        app: definition.app.clone(),
        log_path: log_path.display().to_string(),
        started_at: unix_timestamp(),
        ready,
    })
}

fn process_app_request<'a>(
    context: &'a ProcessContext<'a>,
    app: &'a str,
    args: &'a [String],
    shell: Option<&'a str>,
    environment_policy: EnvironmentPolicy,
) -> AppRequest<'a> {
    AppRequest {
        flake_arg: Some(context.flake.nix_ref.as_str()),
        nix_override: context.nix_override,
        app,
        args,
        root: false,
        cwd: None,
        shell,
        shell_mode: ShellMode::Smart,
        environment_policy,
        nix_flags: context.nix_flags,
        context: None,
    }
}

fn spawn_background(
    program: &Path,
    args: &[String],
    cwd: &Path,
    environment: &EnvironmentPolicy,
    overrides: &BTreeMap<String, String>,
    log_file: File,
) -> io::Result<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file))
            .process_group(0);
        environment.apply_with_overrides(&mut command, overrides);
        let child = command.spawn()?;
        Ok(child.id())
    }

    #[cfg(not(unix))]
    {
        let _ = (program, args, cwd, environment, overrides, log_file);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "background process spawn is not supported on this platform",
        ))
    }
}

fn wait_for_readiness(readiness: Option<&ProcessReadiness>, pid: u32) -> Result<bool, String> {
    let Some(readiness) = readiness else {
        return Ok(true);
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if !is_pid_alive(pid) {
            return Err("process exited before readiness succeeded".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("readiness probe timed out after 30s".to_owned());
        }
        let ready = match readiness {
            ProcessReadiness { tcp: Some(tcp), .. } => probe_tcp(tcp.port),
            ProcessReadiness {
                http: Some(http), ..
            } => probe_http(&http.url)?,
            ProcessReadiness {
                tcp: None,
                http: None,
            } => true,
        };
        if ready {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn probe_tcp(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

fn probe_http(url: &str) -> Result<bool, String> {
    if url.trim().starts_with("https://") {
        return Err(
            "readiness.http.url must use http:// (TLS probes are not implemented)".to_owned(),
        );
    }
    let Ok((host, port, path)) = parse_http_url(url) else {
        return Ok(false);
    };
    use std::net::TcpStream;
    let address = format!("{host}:{port}");
    let Ok(mut stream) = TcpStream::connect(&address) else {
        return Ok(false);
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return Ok(false);
    }
    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return Ok(false);
    }
    Ok(response.contains("HTTP/1.1 200") || response.contains("HTTP/1.0 200"))
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), ()> {
    let without_scheme = url.strip_prefix("http://").ok_or(())?;
    let (host_port, path) = match without_scheme.split_once('/') {
        Some((host_port, rest)) => (host_port, format!("/{rest}")),
        None => (without_scheme, "/".to_owned()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => (host.to_owned(), port.parse().map_err(|_| ())?),
        None => (host_port.to_owned(), 80),
    };
    Ok((host, port, path))
}

fn project_identity(local_root: Option<&camino::Utf8Path>, nix_ref: &str) -> String {
    local_root
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| nix_ref.to_owned())
}

fn state_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "nxr")
        .map(|dirs| dirs.data_local_dir().join("processes"))
}

fn state_path(project_id: &str) -> Option<PathBuf> {
    let dir = state_dir()?;
    let safe = project_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    Some(dir.join(format!("{safe}.json")))
}

fn load_state(project_id: &str) -> Result<ProcessStateFile, ProcessError> {
    let Some(path) = state_path(project_id) else {
        return Ok(empty_state(project_id));
    };
    if !path.is_file() {
        return Ok(empty_state(project_id));
    }
    let contents = fs::read_to_string(path)?;
    let mut state: ProcessStateFile = serde_json::from_str(&contents)?;
    state
        .processes
        .retain(|_, record| is_supervised_process_alive(record));
    Ok(state)
}

fn save_state(project_id: &str, state: &ProcessStateFile) -> Result<(), ProcessError> {
    let Some(path) = state_path(project_id) else {
        return Ok(());
    };
    let contents = serde_json::to_string_pretty(state)?;
    write_state_atomically(&path, contents.as_bytes())?;
    Ok(())
}

fn empty_state(project_id: &str) -> ProcessStateFile {
    ProcessStateFile {
        schema_version: PROCESS_STATE_SCHEMA_VERSION,
        project_id: project_id.to_owned(),
        processes: BTreeMap::new(),
    }
}

fn process_log_path(project_id: &str, name: &str) -> Result<PathBuf, ProcessError> {
    validate_node_id(name).map_err(|error| ProcessError::InvalidName {
        name: name.to_owned(),
        message: process_name_error_message(&error),
    })?;
    let dir = state_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "state directory unavailable"))?;
    let safe_project = project_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    let safe_name = sanitize_process_log_name(name);
    Ok(dir.join(safe_project).join(format!("{safe_name}.log")))
}

fn process_name_error_message(error: &ProcessNameError) -> String {
    match error {
        ProcessNameError::Empty => "name must not be empty".to_owned(),
        ProcessNameError::PathSeparator { .. } => {
            "name must not contain path separators".to_owned()
        }
        ProcessNameError::ParentTraversal { .. } => "name must not contain `..`".to_owned(),
    }
}

fn is_supervised_process_alive(record: &RunningProcessRecord) -> bool {
    if !is_pid_alive(record.pid) {
        return false;
    }
    pid_start_time_matches(record.pid, record.start_time)
}

fn pid_start_time_matches(pid: u32, expected: Option<u64>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    capture_pid_start_time(pid) == Some(expected)
}

fn capture_pid_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_pid_start_time(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_pid_start_time(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_pid_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit(')').next()?.trim();
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    fields.get(20)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_pid_start_time(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stamp = String::from_utf8(output.stdout).ok()?;
    let stamp = stamp.trim();
    if stamp.is_empty() {
        return None;
    }
    Some(fnv1a64(stamp.as_bytes()))
}

// Only referenced from macos_pid_start_time; keep cfg-gated so Linux clippy
// (-D warnings in CI) does not treat it as dead_code.
#[cfg(target_os = "macos")]
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001B3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn write_state_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    use fs2::FileExt;

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    fs::create_dir_all(parent)?;

    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state")
    ));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let temp_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        Ok(())
    })();

    let _ = fs::remove_file(&temp_path);
    lock_file.unlock()?;
    write_result
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn terminate_supervised_process(
    name: &str,
    record: &RunningProcessRecord,
) -> Result<(), ProcessError> {
    if !pid_start_time_matches(record.pid, record.start_time) {
        return Err(ProcessError::IdentityMismatch {
            name: name.to_owned(),
            pid: record.pid,
        });
    }
    terminate_pid(record.pid)
}

fn terminate_pid(pid: u32) -> Result<(), ProcessError> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        let pgid = Pid::from_raw(pid as i32);
        let _ = killpg(pgid, Signal::SIGTERM);
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while Instant::now() < deadline {
            if killpg(pgid, None).is_err() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = killpg(pgid, Signal::SIGKILL);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(ProcessError::Supervision {
            name: "process".to_owned(),
            message: "process shutdown is not supported on this platform".to_owned(),
        })
    }
}

fn unix_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{seconds}")
}

#[cfg(test)]
mod tests {
    use nxr_core::EnvironmentPolicy;
    use nxr_nix::OptionalNixFlags;

    use super::{
        ProcessContext, RunningProcessRecord, probe_http, process_app_request, process_log_path,
        resolve_targets, terminate_supervised_process, wait_for_readiness,
    };
    use crate::flake::FlakeSelection;
    use crate::shell_mode::ShellMode;
    use nxr_task::{
        ProcessDefinition, ProcessNameError, ProcessReadiness, ReadinessTcp, TaskDocument,
        validate_node_id,
    };

    fn sample_context<'a>(nix_ref: &'a str, nix_flags: &'a OptionalNixFlags) -> ProcessContext<'a> {
        ProcessContext {
            flake: FlakeSelection {
                display: nix_ref.to_owned(),
                nix_ref: nix_ref.to_owned(),
                local_root: None,
            },
            nix_override: Some("/custom/nix"),
            nix_flags,
            apps: Vec::new(),
            document: TaskDocument::new(Default::default()),
            project_id: "fixture".to_owned(),
        }
    }

    #[test]
    fn process_app_request_propagates_selected_flake() {
        let nix_flags = OptionalNixFlags::default();
        let context = sample_context("path:/tmp/other-flake", &nix_flags);
        let request =
            process_app_request(&context, "api-dev", &[], None, EnvironmentPolicy::Inherit);
        assert_eq!(request.flake_arg, Some("path:/tmp/other-flake"));
        assert_eq!(request.nix_override, Some("/custom/nix"));
        assert_eq!(request.nix_flags, &nix_flags);
        assert_eq!(request.shell_mode, ShellMode::Smart);
    }

    #[test]
    fn resolve_targets_rejects_unsafe_names() {
        let mut processes = std::collections::BTreeMap::new();
        processes.insert(
            "api".to_owned(),
            ProcessDefinition {
                app: "api".to_owned(),
                depends_on: Vec::new(),
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        let err = resolve_targets(&processes, &["../escape".to_owned()])
            .expect_err("unsafe name rejected");
        assert!(matches!(
            err,
            super::ProcessError::InvalidName { name, .. } if name == "../escape"
        ));
    }

    #[test]
    fn resolve_targets_expands_dependency_closure_in_topo_order() {
        let mut processes = std::collections::BTreeMap::new();
        processes.insert(
            "agentd".to_owned(),
            ProcessDefinition {
                app: "agentd".to_owned(),
                depends_on: vec!["latticed@ready".to_owned()],
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        processes.insert(
            "latticed".to_owned(),
            ProcessDefinition {
                app: "latticed".to_owned(),
                depends_on: Vec::new(),
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        let ordered = resolve_targets(&processes, &["agentd".to_owned()]).expect("topo");
        assert_eq!(ordered, vec!["latticed".to_owned(), "agentd".to_owned()]);
    }

    #[test]
    fn resolve_targets_with_empty_names_orders_all_processes() {
        let mut processes = std::collections::BTreeMap::new();
        processes.insert(
            "agentd".to_owned(),
            ProcessDefinition {
                app: "agentd".to_owned(),
                depends_on: vec!["latticed@ready".to_owned()],
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        processes.insert(
            "latticed".to_owned(),
            ProcessDefinition {
                app: "latticed".to_owned(),
                depends_on: Vec::new(),
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        let ordered = resolve_targets(&processes, &[]).expect("topo order for all processes");
        assert_eq!(ordered, vec!["latticed".to_owned(), "agentd".to_owned()]);
    }

    #[test]
    fn resolve_targets_rejects_dependency_cycle() {
        let mut processes = std::collections::BTreeMap::new();
        processes.insert(
            "a".to_owned(),
            ProcessDefinition {
                app: "a".to_owned(),
                depends_on: vec!["b".to_owned()],
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        processes.insert(
            "b".to_owned(),
            ProcessDefinition {
                app: "b".to_owned(),
                depends_on: vec!["a".to_owned()],
                readiness: None,
                restart: None,
                context: None,
                working_directory: None,
                arguments: Vec::new(),
                shell: None,
            },
        );
        let err = resolve_targets(&processes, &[]).expect_err("cycle rejected");
        assert!(matches!(
            err,
            super::ProcessError::Supervision { message, .. } if message.contains("cycle")
        ));
    }

    #[test]
    fn process_log_path_sanitizes_special_characters() {
        let path = process_log_path("fixture", "api:worker").expect("sanitized path");
        assert!(path.ends_with("api_worker.log"));
    }

    #[test]
    fn terminate_supervised_process_refuses_identity_mismatch() {
        let record = RunningProcessRecord {
            pid: std::process::id(),
            start_time: Some(42),
            app: "api".to_owned(),
            log_path: "/tmp/api.log".to_owned(),
            started_at: "0".to_owned(),
            ready: true,
        };
        let err = terminate_supervised_process("api", &record).expect_err("identity mismatch");
        assert!(matches!(
            err,
            super::ProcessError::IdentityMismatch { name, pid }
                if name == "api" && pid == record.pid
        ));
    }

    #[test]
    fn wait_for_readiness_fails_when_process_exits_before_ready() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn short-lived child");
        let pid = child.id();
        child.wait().expect("child exits promptly");

        let readiness = ProcessReadiness {
            tcp: Some(ReadinessTcp { port: 1 }),
            http: None,
        };
        let err = wait_for_readiness(Some(&readiness), pid).expect_err("dead pid never ready");
        assert!(
            err.contains("exited"),
            "expected exit-before-ready message, got {err}"
        );
    }

    #[test]
    fn wait_for_readiness_without_probe_succeeds_immediately() {
        let ready = wait_for_readiness(None, std::process::id()).expect("no probe means ready");
        assert!(ready);
    }

    #[test]
    fn probe_http_rejects_https_url() {
        let err = probe_http("https://127.0.0.1:8080/health").expect_err("https rejected");
        assert!(err.contains("http://"), "unexpected message: {err}");
    }

    #[test]
    fn validate_node_id_matches_task_style_rules() {
        assert!(validate_node_id("worker").is_ok());
        assert_eq!(
            validate_node_id("a/b"),
            Err(ProcessNameError::PathSeparator {
                name: "a/b".to_owned()
            })
        );
    }
}
