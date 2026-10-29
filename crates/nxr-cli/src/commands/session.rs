//! Local run records. A start launches the given command, or records a remote
//! submit that is that same command. These records are not fleet Profile
//! Assignments.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::runner_output::RunnerOutput;

#[cfg(unix)]
use nix::sys::signal::{Signal, kill, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionRecord {
    id: String,
    label: String,
    status: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    cpu: u32,
    #[serde(default)]
    memory_mib: u64,
    events: Vec<SessionEvent>,
    artifacts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionEvent {
    cursor: u64,
    message: String,
}

/// What `nxr session start` should run.
#[derive(Clone, Debug)]
pub struct SessionStart {
    pub label: String,
    pub command: Vec<String>,
    pub remote: Option<String>,
    pub cpu: u32,
    pub memory_mib: u64,
}

/// CPU tokens and memory the local machine will admit. This is not a cluster pool.
#[derive(Clone, Copy, Debug)]
pub struct LocalBudget {
    pub cpu: u32,
    pub memory_mib: u64,
}

impl LocalBudget {
    /// Read `NXR_LOCAL_CPU` and `NXR_LOCAL_MEMORY_MIB`. Unset means no cap.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            cpu: std::env::var("NXR_LOCAL_CPU")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(u32::MAX),
            memory_mib: std::env::var("NXR_LOCAL_MEMORY_MIB")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(u64::MAX),
        }
    }
}

fn session_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .ok_or_else(|| "session state directory is not set".to_owned())?;
    let dir = base.join("nxr").join("sessions");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir)
}

fn session_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if id.trim().is_empty() || id.contains('/') || id.contains("..") {
        return Err("run id is not a session name".into());
    }
    Ok(dir.join(format!("{id}.json")))
}

fn load(dir: &Path, id: &str) -> Result<SessionRecord, String> {
    let text =
        fs::read_to_string(session_path(dir, id)?).map_err(|err| format!("session {id}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("session json: {err}"))
}

fn store(dir: &Path, record: &SessionRecord) -> Result<(), String> {
    let path = session_path(dir, &record.id)?;
    let body = serde_json::to_string_pretty(record).map_err(|err| err.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body).map_err(|err| err.to_string())?;
    fs::rename(&temporary, &path).map_err(|err| err.to_string())
}

fn records(dir: &Path) -> Result<Vec<SessionRecord>, String> {
    let mut loaded = Vec::new();
    if !dir.is_dir() {
        return Ok(loaded);
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        let id = name.trim_end_matches(".json");
        loaded.push(load(dir, id)?);
    }
    Ok(loaded)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn refresh(record: &mut SessionRecord) {
    let Some(pid) = record.pid else {
        return;
    };
    if record.status != "running" && record.status != "submitted" {
        return;
    }
    if !pid_alive(pid) {
        record.status = "exited".into();
        push_event(record, "exited");
        record.pid = None;
    }
}

fn push_event(record: &mut SessionRecord, message: &str) {
    let cursor = record.events.last().map(|event| event.cursor).unwrap_or(0) + 1;
    record.events.push(SessionEvent {
        cursor,
        message: message.to_owned(),
    });
}

fn reserved(dir: &Path) -> Result<(u32, u64), String> {
    let mut cpu = 0_u32;
    let mut memory = 0_u64;
    for mut record in records(dir)? {
        refresh(&mut record);
        if record.status == "running" || record.status == "submitted" {
            cpu = cpu.saturating_add(record.cpu);
            memory = memory.saturating_add(record.memory_mib);
            store(dir, &record)?;
        } else if record.status == "exited" {
            store(dir, &record)?;
        }
    }
    Ok((cpu, memory))
}

fn spawn_command(
    command: &[String],
    cache_env: &std::collections::BTreeMap<String, String>,
) -> Result<std::process::Child, String> {
    let Some(program) = command.first() else {
        return Err("session start needs the command to run".into());
    };
    let mut child = Command::new(program);
    child.args(&command[1..]);
    child.stdin(Stdio::null());
    child.stdout(Stdio::null());
    child.stderr(Stdio::null());
    nxr_task::compiler_cache::apply_compiler_cache(
        &mut child,
        cache_env,
        &std::collections::BTreeMap::new(),
    );
    #[cfg(unix)]
    {
        child.process_group(0);
    }
    child
        .spawn()
        .map_err(|err| format!("spawn {program}: {err}"))
}

fn watch_exit(dir: PathBuf, id: String, mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
        let Ok(mut record) = load(&dir, &id) else {
            return;
        };
        if record.status == "running" || record.status == "submitted" {
            record.status = "exited".into();
            record.pid = None;
            push_event(&mut record, "exited");
            let _ = store(&dir, &record);
        }
    });
}

/// Launch `request` under `budget` and store the run under the session directory.
///
/// # Errors
///
/// Returns an error when the command is missing, the local budget would be
/// exceeded, or the process cannot be spawned.
pub fn start(
    request: &SessionStart,
    budget: LocalBudget,
    runner: RunnerOutput,
) -> Result<(), String> {
    start_at(
        &session_dir()?,
        request,
        budget,
        &nxr_task::compiler_cache::compiler_cache_from_process(),
        runner,
    )
}

pub(crate) fn start_at(
    dir: &Path,
    request: &SessionStart,
    budget: LocalBudget,
    cache_env: &std::collections::BTreeMap<String, String>,
    runner: RunnerOutput,
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    if request.command.is_empty() {
        return Err("session start needs the command to run".into());
    }
    let id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let (used_cpu, used_memory) = reserved(dir)?;
    if used_cpu.saturating_add(request.cpu) > budget.cpu
        || used_memory.saturating_add(request.memory_mib) > budget.memory_mib
    {
        let mut refused = SessionRecord {
            id: id.clone(),
            label: request.label.clone(),
            status: "refused".into(),
            kind: if request.remote.is_some() {
                "remote".into()
            } else {
                "local".into()
            },
            command: request.command.clone(),
            remote: request.remote.clone(),
            pid: None,
            cpu: request.cpu,
            memory_mib: request.memory_mib,
            events: Vec::new(),
            artifacts: Vec::new(),
        };
        push_event(&mut refused, "refused");
        store(dir, &refused)?;
        runner.info(&id).map_err(|err| err.to_string())?;
        return Err("local budget exceeded".into());
    }
    let child = spawn_command(&request.command, cache_env)?;
    let pid = child.id();
    let kind = if request.remote.is_some() {
        "remote"
    } else {
        "local"
    };
    let mut record = SessionRecord {
        id: id.clone(),
        label: request.label.clone(),
        status: "running".into(),
        kind: kind.into(),
        command: request.command.clone(),
        remote: request.remote.clone(),
        pid: Some(pid),
        cpu: request.cpu,
        memory_mib: request.memory_mib,
        events: Vec::new(),
        artifacts: Vec::new(),
    };
    let started = if request.remote.is_some() {
        "submitted"
    } else {
        "started"
    };
    push_event(&mut record, started);
    push_event(
        &mut record,
        &format!(
            "compiler-cache {}",
            nxr_task::compiler_cache::compiler_cache_mode(cache_env)
        ),
    );
    store(dir, &record)?;
    watch_exit(dir.to_path_buf(), id.clone(), child);
    runner.info(&id).map_err(|err| err.to_string())
}

/// Print one line for each stored run.
///
/// # Errors
///
/// Returns an error when a session file cannot be read.
pub fn list(runner: RunnerOutput) -> Result<(), String> {
    list_at(&session_dir()?, runner)
}

pub(crate) fn list_at(dir: &Path, runner: RunnerOutput) -> Result<(), String> {
    let mut loaded = records(dir)?;
    for record in &mut loaded {
        refresh(record);
        store(dir, record)?;
        runner
            .info(format!("{} {} {}", record.id, record.status, record.label))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Print events whose cursor is greater than `after`.
///
/// # Errors
///
/// Returns an error when the run cannot be read.
pub fn events(id: &str, after: u64, runner: RunnerOutput) -> Result<(), String> {
    events_at(&session_dir()?, id, after, runner)
}

pub(crate) fn events_at(
    dir: &Path,
    id: &str,
    after: u64,
    runner: RunnerOutput,
) -> Result<(), String> {
    let mut record = load(dir, id)?;
    refresh(&mut record);
    store(dir, &record)?;
    for event in record.events.iter().filter(|event| event.cursor > after) {
        runner
            .info(format!("{} {}", event.cursor, event.message))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Print the run status, declared resources, and artifact references.
///
/// # Errors
///
/// Returns an error when the run cannot be read.
pub fn inspect(id: &str, runner: RunnerOutput) -> Result<(), String> {
    inspect_at(&session_dir()?, id, runner)
}

pub(crate) fn inspect_at(dir: &Path, id: &str, runner: RunnerOutput) -> Result<(), String> {
    let mut record = load(dir, id)?;
    refresh(&mut record);
    store(dir, &record)?;
    runner
        .info(format!(
            "{} {} cpu {} memory {} artifacts {}",
            record.status,
            record.label,
            record.cpu,
            record.memory_mib,
            if record.artifacts.is_empty() {
                "none".into()
            } else {
                record.artifacts.join(",")
            }
        ))
        .map_err(|err| err.to_string())
}

/// Stop the run's process and record `cancelled`.
///
/// # Errors
///
/// Returns an error when the run is not running or the signal cannot be sent.
pub fn cancel(id: &str, runner: RunnerOutput) -> Result<(), String> {
    cancel_at(&session_dir()?, id, runner)
}

pub(crate) fn cancel_at(dir: &Path, id: &str, runner: RunnerOutput) -> Result<(), String> {
    let mut record = load(dir, id)?;
    refresh(&mut record);
    if record.status != "running" && record.status != "submitted" {
        store(dir, &record)?;
        return Err(format!("run {id} is {}", record.status));
    }
    if let Some(pid) = record.pid {
        stop_pid(pid)?;
    }
    record.status = "cancelled".into();
    record.pid = None;
    push_event(&mut record, "cancelled");
    store(dir, &record)?;
    runner
        .info(format!("{id} cancelled"))
        .map_err(|err| err.to_string())
}

fn stop_pid(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let group = Pid::from_raw(pid as i32);
        let _ = killpg(group, Signal::SIGTERM);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if killpg(group, None).is_err() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = killpg(group, Signal::SIGKILL);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err("process cancellation is not available on this build".into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use super::*;
    use crate::output_options::{ColorWhen, LogFormat, OutputOptions};

    fn quiet() -> RunnerOutput {
        RunnerOutput::new(OutputOptions::new(
            1,
            0,
            false,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ))
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut file = fs::File::create(&path).unwrap();
        write!(file, "{body}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn a_local_command_exits_and_a_cursor_replays_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("ran");
        let program = script(
            root.path(),
            "app.sh",
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );
        let mut cache = BTreeMap::new();
        cache.insert("CI".to_owned(), "true".to_owned());
        cache.insert(
            "SCCACHE_ENDPOINT".to_owned(),
            "https://cache.example".to_owned(),
        );
        start_at(
            root.path(),
            &SessionStart {
                label: "local".into(),
                command: vec![program.display().to_string()],
                remote: None,
                cpu: 1,
                memory_mib: 32,
            },
            LocalBudget {
                cpu: 2,
                memory_mib: 64,
            },
            &cache,
            quiet(),
        )
        .unwrap();
        let id = records(root.path()).unwrap()[0].id.clone();
        let mut record = load(root.path(), &id).unwrap();
        for _ in 0..50 {
            if marker.is_file() && record.status == "exited" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            record = load(root.path(), &id).unwrap();
        }
        assert!(marker.is_file());
        assert_eq!(record.status, "exited");
        assert!(record.events.iter().any(|event| event.message == "started"));
        assert!(
            record
                .events
                .iter()
                .any(|event| event.message == "compiler-cache sccache+remote")
        );
        assert!(
            record
                .events
                .iter()
                .all(|event| !event.message.contains("cache.example"))
        );
        events_at(root.path(), &id, 1, quiet()).unwrap();
        let replayed = load(root.path(), &id).unwrap();
        assert!(replayed.events.iter().any(|event| event.cursor > 1));
    }

    #[test]
    fn a_remote_submit_can_be_cancelled_and_a_second_start_can_be_refused() {
        let root = tempfile::tempdir().unwrap();
        let program = script(root.path(), "sleep.sh", "#!/bin/sh\nsleep 30\n");
        start_at(
            root.path(),
            &SessionStart {
                label: "remote".into(),
                command: vec![program.display().to_string()],
                remote: Some("fixture-host".into()),
                cpu: 1,
                memory_mib: 16,
            },
            LocalBudget {
                cpu: 1,
                memory_mib: 16,
            },
            &BTreeMap::new(),
            quiet(),
        )
        .unwrap();
        let id = records(root.path()).unwrap()[0].id.clone();
        let running = load(root.path(), &id).unwrap();
        assert_eq!(running.kind, "remote");
        assert_eq!(running.status, "running");
        assert!(
            running
                .events
                .iter()
                .any(|event| event.message == "submitted")
        );
        let error = start_at(
            root.path(),
            &SessionStart {
                label: "overflow".into(),
                command: vec![program.display().to_string()],
                remote: None,
                cpu: 1,
                memory_mib: 1,
            },
            LocalBudget {
                cpu: 1,
                memory_mib: 16,
            },
            &BTreeMap::new(),
            quiet(),
        )
        .unwrap_err();
        assert!(error.contains("local budget exceeded"), "{error}");
        cancel_at(root.path(), &id, quiet()).unwrap();
        let cancelled = load(root.path(), &id).unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(cancelled.pid.is_none());
        list_at(root.path(), quiet()).unwrap();
    }
}
