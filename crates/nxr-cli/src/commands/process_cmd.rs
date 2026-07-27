//! `nxr up` / `status` / `logs` / `down` for long-running process nodes.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nxr_core::EnvironmentPolicy;
use nxr_core::diagnostics::exit;
use nxr_nix::{OptionalNixFlags, resolve_app_by_name};
use nxr_task::{ProcessDefinition, ProcessReadiness};
use serde::{Deserialize, Serialize};

use crate::commands::common::{AppRequest, PrepareError, WorkspaceSnapshot, prepare_fast_app_plan};
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
            Self::Supervision { .. } => exit::PROCESS_SUPERVISION,
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
    let targets = resolve_targets(&context.document.processes, names)?;
    let mut state = load_state(&context.project_id)?;

    for name in targets {
        if state.processes.contains_key(&name) {
            let pid = state.processes[&name].pid;
            return Err(ProcessError::AlreadyRunning { name, pid });
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
    }

    save_state(&context.project_id, &state)?;
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
            let alive = is_pid_alive(record.pid);
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

    let mut file = File::open(&record.log_path)?;
    let mut stdout = io::stdout().lock();
    if follow {
        let mut buffer = [0u8; 4096];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                if !is_pid_alive(record.pid) {
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
        terminate_pid(record.pid)?;
    }

    save_state(&context.project_id, &state)?;
    Ok(exit::SUCCESS)
}

struct ProcessContext {
    flake: crate::flake::FlakeSelection,
    apps: Vec<nxr_core::App>,
    document: nxr_task::TaskDocument,
    project_id: String,
}

fn discover_process_context(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
) -> Result<ProcessContext, ProcessError> {
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
        apps: snapshot.apps.values().cloned().collect(),
        document,
        project_id,
    })
}

fn resolve_targets(
    processes: &BTreeMap<String, ProcessDefinition>,
    names: &[String],
) -> Result<Vec<String>, ProcessError> {
    if names.is_empty() {
        return Ok(processes.keys().cloned().collect());
    }
    for name in names {
        if !processes.contains_key(name) {
            return Err(ProcessError::NotFound { name: name.clone() });
        }
    }
    Ok(names.to_vec())
}

fn dependencies_ready(
    definition: &ProcessDefinition,
    running: &BTreeMap<String, RunningProcessRecord>,
) -> bool {
    definition.depends_on.iter().all(|dependency| {
        let base = dependency.split('@').next().unwrap_or(dependency);
        running
            .get(base)
            .is_some_and(|record| record.ready || !dependency.contains("@ready"))
    })
}

fn spawn_process(
    context: &ProcessContext,
    name: &str,
    definition: &ProcessDefinition,
) -> Result<RunningProcessRecord, ProcessError> {
    let _app = resolve_app_by_name(&context.apps, &definition.app).map_err(|error| {
        ProcessError::Supervision {
            name: name.to_owned(),
            message: error.to_string(),
        }
    })?;
    let environment_policy = EnvironmentPolicy::Inherit;
    let app_request = AppRequest {
        flake_arg: None,
        nix_override: None,
        app: definition.app.as_str(),
        args: &[],
        root: false,
        cwd: None,
        shell: None,
        shell_mode: ShellMode::Smart,
        environment_policy: environment_policy.clone(),
        nix_flags: &OptionalNixFlags::default(),
    };
    let prepared = prepare_fast_app_plan(&app_request)?;

    let log_path = process_log_path(&context.project_id, name)?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let pid = spawn_background(
        prepared.nix.as_std_path(),
        &prepared.plan.command.arguments,
        prepared.execution_directory.as_std_path(),
        &prepared.plan.environment_policy,
        log_file,
    )
    .map_err(|error| ProcessError::Supervision {
        name: name.to_owned(),
        message: error.to_string(),
    })?;

    let ready = wait_for_readiness(definition.readiness.as_ref());
    Ok(RunningProcessRecord {
        pid,
        app: definition.app.clone(),
        log_path: log_path.display().to_string(),
        started_at: unix_timestamp(),
        ready,
    })
}

fn spawn_background(
    program: &Path,
    args: &[String],
    cwd: &Path,
    environment: &EnvironmentPolicy,
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
        environment.apply_with_overrides(&mut command, &BTreeMap::new());
        let child = command.spawn()?;
        Ok(child.id())
    }

    #[cfg(not(unix))]
    {
        let _ = (program, args, cwd, environment, log_file);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "background process spawn is not supported on this platform",
        ))
    }
}

fn wait_for_readiness(readiness: Option<&ProcessReadiness>) -> bool {
    let Some(readiness) = readiness else {
        return true;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= deadline {
            return false;
        }
        let ready = match readiness {
            ProcessReadiness { tcp: Some(tcp), .. } => probe_tcp(tcp.port),
            ProcessReadiness {
                http: Some(http), ..
            } => probe_http(&http.url),
            ProcessReadiness {
                tcp: None,
                http: None,
            } => true,
        };
        if ready {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn probe_tcp(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

fn probe_http(url: &str) -> bool {
    let Ok((host, port, path)) = parse_http_url(url) else {
        return false;
    };
    use std::net::TcpStream;
    let address = format!("{host}:{port}");
    let Ok(mut stream) = TcpStream::connect(&address) else {
        return false;
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return false;
    }
    response.contains("HTTP/1.1 200") || response.contains("HTTP/1.0 200")
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), ()> {
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or(())?;
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
    state.processes.retain(|_, record| is_pid_alive(record.pid));
    Ok(state)
}

fn save_state(project_id: &str, state: &ProcessStateFile) -> Result<(), ProcessError> {
    let Some(path) = state_path(project_id) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(state)?;
    fs::write(path, contents)?;
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
    let dir = state_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "state directory unavailable"))?;
    let safe_project = project_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    Ok(dir.join(safe_project).join(format!("{name}.log")))
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
