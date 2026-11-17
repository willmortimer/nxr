//! Process owner for `nxr session`.
//!
//! The daemon that accepts the run is the parent. Cache methods on the same
//! socket stay optional, and `nix run` does not call this module. A restarted
//! daemon reconciles pid and start time; a recycled pid is not the old run.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::durable_run::{self, DurableRunStatus};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const CACHE_KEYS: [&str; 3] = ["SCCACHE_ENDPOINT", "SCCACHE_BUCKET", "CARGO_INCREMENTAL"];

/// Upper bound on reconcile passes per admission call (crash/restart safety valve).
const MAX_RECONCILE_PASSES: u32 = 8;

#[cfg(test)]
static TEST_WAIT_THREADS: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn join_test_wait_threads() {
    let handles = std::mem::take(&mut *TEST_WAIT_THREADS.lock().expect("wait thread mutex"));
    for handle in handles {
        let _ = handle.join();
    }
}

static TARGET_GUARDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<crate::target_lock::TargetAdmission>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn hold_targets(run_id: &str, admissions: Vec<crate::target_lock::TargetAdmission>) {
    if admissions.is_empty() {
        return;
    }
    TARGET_GUARDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(run_id.to_owned(), admissions);
}

fn release_target(run_id: &str) {
    TARGET_GUARDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(run_id);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnedRun {
    id: String,
    label: String,
    status: String,
    kind: String,
    command: Vec<String>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    attempt_id: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    start_time: Option<u64>,
    #[serde(default)]
    cpu: u32,
    #[serde(default)]
    memory_mib: u64,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    log_path: String,
    /// Opaque id of the workspace reservation. Empty on records written before this field.
    #[serde(default)]
    admission_id: String,
    /// Canonical Cargo target directory for a local build. Empty when unset.
    #[serde(default)]
    target_dir: String,
    /// Operation this run was started for. Empty on records written before this field.
    #[serde(default)]
    operation_id: String,
    /// Workspace this run was started for. Empty on records written before this field.
    #[serde(default)]
    workspace_id: String,
    events: Vec<RunEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunEvent {
    cursor: u64,
    message: String,
}

/// Canonical directory identity for one Cargo target.
///
/// An existing directory uses its canonical path. A missing leaf uses the
/// canonical parent plus that leaf so two spellings of the same target match.
#[must_use]
pub fn canonical_target_dir(path: &Path) -> String {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical.display().to_string();
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && let Ok(parent) = fs::canonicalize(parent)
        && let Some(name) = path.file_name()
    {
        return parent.join(name).display().to_string();
    }
    path.display().to_string()
}

/// Directory of run records for the daemon socket.
#[must_use]
pub fn runs_dir(socket: &Path) -> PathBuf {
    socket
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("supervisor")
}

/// Start `params.command` as a child of this process.
///
/// # Errors
///
/// Returns an error when the command is missing, the budget is already taken,
/// or the process cannot be spawned. A refused run is stored and does not
/// reserve capacity.
pub fn start(dir: &Path, params: &Value) -> Result<Value, String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    reconcile_with_limit(dir, MAX_RECONCILE_PASSES)?;
    let command = string_list(params, "command")?;
    if command.is_empty() {
        return Err("session start needs the command to run".into());
    }
    let label = params
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("session")
        .to_owned();
    let admission_id = params
        .get("admission_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let remote = optional_string(params, "remote");
    let attempt_id = optional_string(params, "attempt_id");
    let cpu = params.get("cpu").and_then(Value::as_u64).unwrap_or(0) as u32;
    let memory_mib = params
        .get("memory_mib")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cpu_budget = params
        .get("cpu_budget")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(u32::MAX)) as u32;
    let memory_budget = params
        .get("memory_budget")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let cache_mode = params
        .get("cache_mode")
        .and_then(Value::as_str)
        .unwrap_or("off")
        .to_owned();
    if cache_mode.contains("://") || cache_mode.contains('@') {
        return Err("compiler cache mode must not contain an endpoint".into());
    }
    let cache_env = cache_assignments(params.get("cache_env"))?;
    let id = new_id();
    let target_dir = params
        .get("target_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| canonical_target_dir(Path::new(value)))
        .unwrap_or_default();
    let operation_id = params
        .get("operation_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let workspace_id = params
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    with_lock(dir, || {
        if !target_dir.is_empty() {
            for existing in records(dir)? {
                if existing.status == "running"
                    && existing.kind == "local"
                    && existing.target_dir == target_dir
                {
                    return Err(format!(
                        "cargo target {target_dir} already has a running local build"
                    ));
                }
            }
        }
        let (used_cpu, used_memory) = reserved(dir)?;
        if used_cpu.saturating_add(cpu) > cpu_budget
            || used_memory.saturating_add(memory_mib) > memory_budget
        {
            let mut refused = OwnedRun {
                id: id.clone(),
                label,
                status: "refused".into(),
                kind: kind_of(remote.as_ref()),
                command,
                remote,
                attempt_id,
                pid: None,
                start_time: None,
                cpu,
                memory_mib,
                exit_code: None,
                log_path: String::new(),
                admission_id: admission_id.clone(),
                target_dir: target_dir.clone(),
                operation_id: operation_id.clone(),
                workspace_id: workspace_id.clone(),
                events: Vec::new(),
            };
            push_event(&mut refused, "refused");
            store(dir, &refused)?;
            return Err("local budget exceeded".into());
        }
        let output_dirs = output_directories(params, &target_dir);
        if remote.is_none() {
            refuse_when_disk_is_short(&output_dirs)?;
        }
        let admissions = if output_dirs.is_empty() || remote.is_some() {
            Vec::new()
        } else {
            crate::resource_guard::acquire_ordered(&output_dirs)?
        };
        let log_path = dir.join(format!("{id}.log"));
        let log = File::create(&log_path).map_err(|err| err.to_string())?;
        let stderr = log.try_clone().map_err(|err| err.to_string())?;
        let mut child = Command::new(&command[0]);
        child.args(&command[1..]);
        if let Some(cwd) = params.get("cwd").and_then(Value::as_str)
            && !cwd.is_empty()
        {
            child.current_dir(cwd);
        }
        child.stdin(Stdio::null());
        child.stdout(log);
        child.stderr(stderr);
        for (key, value) in &cache_env {
            child.env(key, value);
        }
        #[cfg(unix)]
        {
            child.process_group(0);
        }
        capture_child_probe(dir, &id, params, &cache_env);
        let mut spawned = match child.spawn() {
            Ok(child) => child,
            Err(err) => {
                drop(admissions);
                return Err(format!("spawn {}: {err}", command[0]));
            }
        };
        let pid = spawned.id();
        let start_time = capture_start_time(pid);
        let mut record = OwnedRun {
            id: id.clone(),
            label,
            status: "running".into(),
            kind: kind_of(remote.as_ref()),
            command,
            remote,
            attempt_id,
            pid: Some(pid),
            start_time,
            cpu,
            memory_mib,
            exit_code: None,
            log_path: log_path.display().to_string(),
            admission_id,
            target_dir: target_dir.clone(),
            operation_id,
            workspace_id,
            events: Vec::new(),
        };
        for path in &output_dirs {
            let _ = crate::resource_guard::spawn_guardian(&path.join(".nxr-target.lock"), pid);
        }
        hold_targets(&id, admissions);
        push_event(&mut record, "started");
        if record.kind == "remote" {
            push_event(&mut record, "submitted");
        }
        if cache_mode != "off" {
            push_event(&mut record, &format!("compiler-cache {cache_mode}"));
        }
        if let Err(err) = store(dir, &record) {
            release_target(&id);
            let _ = stop_group(pid);
            return Err(err);
        }
        let command_for_durable = record.command.clone();
        durable_run::sync_supervisor_started(&id, &command_for_durable).map_err(|err| {
            release_target(&id);
            let _ = stop_group(pid);
            let _ = fs::remove_file(dir.join(format!("{id}.json")));
            let _ = fs::remove_file(&log_path);
            durable_run::remove_durable_run_artifacts(&id);
            format!("durable run sync: {err}")
        })?;
        let wait_dir = dir.to_path_buf();
        let wait_id = id.clone();
        let wait_pid = pid;
        // Retained under test so suites can join; detached in non-test builds.
        #[cfg_attr(not(test), allow(unused_variables))]
        let wait_handle = thread::spawn(move || {
            let code = spawned
                .wait()
                .ok()
                .and_then(|status| status.code())
                .unwrap_or(-1);
            let _ = with_lock(&wait_dir, || {
                let Ok(mut stored) = load(&wait_dir, &wait_id) else {
                    return Ok(());
                };
                stored.exit_code = Some(code);
                if stored.status == "running" {
                    stored.status = "exited".into();
                    push_event(&mut stored, "exited");
                }
                store(&wait_dir, &stored)?;
                let durable_status = if code == 0 {
                    DurableRunStatus::Succeeded
                } else {
                    DurableRunStatus::Failed
                };
                let _ = durable_run::sync_supervisor_terminal_with_reconcile(
                    &wait_id,
                    durable_status,
                    stored.exit_code,
                );
                Ok(())
            });
            let _ = release_target_if_stopped(&wait_id, Some(wait_pid));
        });
        #[cfg(test)]
        TEST_WAIT_THREADS
            .lock()
            .expect("wait thread mutex")
            .push(wait_handle);
        Ok(json!({ "id": id }))
    })
}

/// Read one run.
///
/// # Errors
///
/// Returns an error when the id is missing or the record cannot be read.
pub fn inspect(dir: &Path, id: &str) -> Result<Value, String> {
    let record = load(dir, id)?;
    serde_json::to_value(record).map_err(|err| err.to_string())
}

/// Events whose cursor is greater than `after`.
///
/// # Errors
///
/// Returns an error when the run cannot be read.
pub fn events(dir: &Path, id: &str, after: u64) -> Result<Value, String> {
    let record = load(dir, id)?;
    let events: Vec<&RunEvent> = record
        .events
        .iter()
        .filter(|event| event.cursor > after)
        .collect();
    serde_json::to_value(events).map_err(|err| err.to_string())
}

/// Log text written by the child, capped at 256 KiB.
///
/// # Errors
///
/// Returns an error when the run or the log cannot be read.
pub fn logs(dir: &Path, id: &str) -> Result<Value, String> {
    let record = load(dir, id)?;
    let bytes = fs::read(&record.log_path).unwrap_or_default();
    let start = bytes.len().saturating_sub(256 * 1024);
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    Ok(json!({ "text": text }))
}

/// Every stored run.
///
/// # Errors
///
/// Returns an error when a record cannot be read.
pub fn list(dir: &Path) -> Result<Value, String> {
    reconcile_with_limit(dir, MAX_RECONCILE_PASSES)?;
    let records = records(dir)?;
    serde_json::to_value(records).map_err(|err| err.to_string())
}

/// Stop the process group of a run.
///
/// A remote run is not stopped here until `remote_confirmed` is set. Killing
/// only the local client is not a successful remote cancel.
///
/// # Errors
///
/// Returns an error when the run is missing, the remote cancel was not
/// confirmed, or the signal fails.
pub fn cancel(dir: &Path, id: &str, remote_confirmed: bool) -> Result<Value, String> {
    reconcile_with_limit(dir, MAX_RECONCILE_PASSES)?;
    with_lock(dir, || {
        let mut record = load(dir, id)?;
        if record.attempt_id.is_some() && !remote_confirmed {
            return Err(
                "remote cancel requires cancel_execution before the local client is stopped".into(),
            );
        }
        if record.status == "running" {
            if let Some(pid) = record.pid {
                stop_group(pid)?;
            }
            record.status = "cancelled".into();
            push_event(&mut record, "cancelled");
            store(dir, &record)?;
            if let Some(pid) = record.pid {
                release_target_if_stopped(id, Some(pid))?;
            } else {
                release_target(id);
            }
            let _ = durable_run::sync_supervisor_terminal_with_reconcile(
                id,
                DurableRunStatus::Cancelled,
                None,
            );
        }
        serde_json::to_value(record).map_err(|err| err.to_string())
    })
}

/// Mark running records whose pid is gone or whose start time no longer matches.
///
/// # Errors
///
/// Returns an error when a record cannot be read or written.
pub fn reconcile(dir: &Path) -> Result<(), String> {
    reconcile_with_limit(dir, MAX_RECONCILE_PASSES)
}

fn reconcile_with_limit(dir: &Path, max_passes: u32) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    for _ in 0..max_passes {
        let changed = reconcile_once(dir)?;
        if !changed {
            break;
        }
    }
    Ok(())
}

fn reconcile_once(dir: &Path) -> Result<bool, String> {
    with_lock(dir, || {
        let mut changed = false;
        for mut record in records(dir)? {
            if record.status != "running" {
                continue;
            }
            let still = match record.pid {
                Some(pid) => !matches!(
                    crate::resource_guard::writer_group_alive(pid, record.start_time),
                    crate::resource_guard::GroupLiveness::Dead
                ),
                None => false,
            };
            if !still {
                record.status = "lost".into();
                record.exit_code = None;
                push_event(&mut record, "lost");
                store(dir, &record)?;
                release_target(&record.id);
                changed = true;
            }
        }
        Ok(changed)
    })
}

fn kind_of(remote: Option<&String>) -> String {
    if remote.is_some() {
        "remote".into()
    } else {
        "local".into()
    }
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("run-{nanos}")
}

fn string_list(params: &Value, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = params.get(field) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(format!("{field} must be a list"));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{field} entries must be strings"))
        })
        .collect()
}

fn optional_string(params: &Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn cache_assignments(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(object) = value.as_object() else {
        return Err("cache_env must be an object".into());
    };
    let mut assigned = Vec::new();
    for key in CACHE_KEYS {
        if let Some(raw) = object.get(key).and_then(Value::as_str)
            && !raw.is_empty()
        {
            assigned.push((key.to_owned(), raw.to_owned()));
        }
    }
    Ok(assigned)
}

fn with_lock<T>(dir: &Path, body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let path = dir.join("admission.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    file.lock_exclusive().map_err(|err| err.to_string())?;
    let result = body();
    let _ = file.unlock();
    result
}

fn reserved(dir: &Path) -> Result<(u32, u64), String> {
    let mut cpu = 0_u32;
    let mut memory = 0_u64;
    for record in records(dir)? {
        if record.status == "running" {
            cpu = cpu.saturating_add(record.cpu);
            memory = memory.saturating_add(record.memory_mib);
        }
    }
    Ok((cpu, memory))
}

fn records(dir: &Path) -> Result<Vec<OwnedRun>, String> {
    let mut names = Vec::new();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(dir).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| load(dir, name.trim_end_matches(".json")))
        .collect()
}

fn load(dir: &Path, id: &str) -> Result<OwnedRun, String> {
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return Err("run id is not a session name".into());
    }
    let text = fs::read_to_string(dir.join(format!("{id}.json")))
        .map_err(|err| format!("session {id}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("session json: {err}"))
}

fn store(dir: &Path, record: &OwnedRun) -> Result<(), String> {
    let path = dir.join(format!("{}.json", record.id));
    let body = serde_json::to_string_pretty(record).map_err(|err| err.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body).map_err(|err| err.to_string())?;
    fs::rename(&temporary, &path).map_err(|err| err.to_string())
}

fn push_event(record: &mut OwnedRun, message: &str) {
    let cursor = record.events.last().map(|event| event.cursor).unwrap_or(0) + 1;
    record.events.push(RunEvent {
        cursor,
        message: message.to_owned(),
    });
}

fn output_directories(params: &Value, target_dir: &str) -> Vec<PathBuf> {
    if let Some(items) = params.get("output_dirs").and_then(Value::as_array) {
        let paths: Vec<PathBuf> = items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(canonical_target_dir(Path::new(value))))
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    if target_dir.is_empty() {
        Vec::new()
    } else {
        vec![PathBuf::from(target_dir)]
    }
}

fn refuse_when_disk_is_short(paths: &[PathBuf]) -> Result<(), String> {
    const RESERVE_BYTES: u64 = 1024 * 1024;
    for path in paths {
        let decision = crate::disk_preflight::disk_preflight(path, RESERVE_BYTES);
        if !decision.allowed {
            return Err(decision.reason);
        }
    }
    Ok(())
}

fn capture_child_probe(dir: &Path, run_id: &str, params: &Value, cache_env: &[(String, String)]) {
    let Some(script) = params
        .get("probe_script")
        .and_then(Value::as_str)
        .filter(|script| !script.is_empty())
    else {
        return;
    };
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);
    if let Some(cwd) = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        command.current_dir(cwd);
    }
    for (key, value) in cache_env {
        command.env(key, value);
    }
    let Ok(output) = command.output() else {
        return;
    };
    if !output.status.success() || output.stdout.is_empty() {
        return;
    }
    let dest = dir.join("provenance");
    if fs::create_dir_all(&dest).is_err() {
        return;
    }
    let _ = fs::write(dest.join(format!("{run_id}.probe")), &output.stdout);
}

/// Drop the in-process guard only after the writer group is dead.
///
/// Uncertain or still-alive groups keep the guard. The caller must not report
/// cleanup as finished in those cases.
fn release_target_if_stopped(run_id: &str, pgid: Option<u32>) -> Result<(), String> {
    let Some(pgid) = pgid else {
        release_target(run_id);
        return Ok(());
    };
    for _ in 0..40 {
        match crate::resource_guard::writer_group_alive(pgid, None) {
            crate::resource_guard::GroupLiveness::Dead => {
                release_target(run_id);
                return Ok(());
            }
            crate::resource_guard::GroupLiveness::Uncertain => {
                return Err("writer liveness is uncertain".into());
            }
            crate::resource_guard::GroupLiveness::Alive => {
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    if crate::resource_guard::writer_group_alive(pgid, None)
        == crate::resource_guard::GroupLiveness::Dead
    {
        release_target(run_id);
        Ok(())
    } else {
        Err("writer is still running".into())
    }
}

#[cfg(test)]
fn pid_alive(pid: u32) -> bool {
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

fn capture_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let end = stat.rfind(')')?;
        let field = stat[end + 1..].split_whitespace().nth(19)?;
        field.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn stop_group(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;
        let group = Pid::from_raw(pid as i32);
        killpg(group, Signal::SIGTERM).map_err(|err| format!("stop {pid}: {err}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err("process-group cancellation is not available on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IsolatedDurableRuns {
        _dir: tempfile::TempDir,
    }

    impl IsolatedDurableRuns {
        fn install() -> Self {
            let dir = tempfile::tempdir().unwrap();
            crate::durable_run::set_test_durable_runs_dir(Some(dir.path().to_path_buf()));
            Self { _dir: dir }
        }
    }

    impl Drop for IsolatedDurableRuns {
        fn drop(&mut self) {
            join_test_wait_threads();
            crate::durable_run::set_test_durable_runs_dir(None);
        }
    }

    fn params(command: Vec<String>, cpu_budget: u32) -> Value {
        json!({
            "label": "local",
            "command": command,
            "cpu": 1,
            "memory_mib": 16,
            "cpu_budget": cpu_budget,
            "memory_budget": 64,
            "cache_mode": "sccache+remote",
            "cache_env": { "SCCACHE_ENDPOINT": "https://cache.example" }
        })
    }

    #[cfg(unix)]
    #[test]
    fn a_finished_command_keeps_its_exit_code_and_log_after_start_returns() {
        let _durable = IsolatedDurableRuns::install();
        let root = tempfile::tempdir().unwrap();
        let started = start(
            root.path(),
            &params(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo supervisor-log; exit 3".into(),
                ],
                2,
            ),
        )
        .unwrap();
        let id = started["id"].as_str().unwrap().to_owned();
        let mut record = inspect(root.path(), &id).unwrap();
        for _ in 0..50 {
            if record["status"] == "exited" {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
            record = inspect(root.path(), &id).unwrap();
        }
        assert_eq!(record["status"], "exited");
        assert_eq!(record["exit_code"], 3);
        let messages: Vec<&str> = record["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["message"].as_str())
            .collect();
        assert!(messages.contains(&"started"));
        assert!(messages.contains(&"compiler-cache sccache+remote"));
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("cache.example"))
        );
        let text = logs(root.path(), &id).unwrap();
        assert!(text["text"].as_str().unwrap().contains("supervisor-log"));
        let replay = events(root.path(), &id, 1).unwrap();
        assert!(
            replay
                .as_array()
                .unwrap()
                .iter()
                .any(|event| { event["cursor"].as_u64().unwrap() > 1 })
        );
    }

    #[cfg(unix)]
    #[test]
    fn one_lock_refuses_the_second_start_and_cancel_clears_the_group() {
        let _durable = IsolatedDurableRuns::install();
        let root = tempfile::tempdir().unwrap();
        let started = start(
            root.path(),
            &json!({
                "label": "remote",
                "command": ["/bin/sh", "-c", "sleep 30 & sleep 30"],
                "remote": "fixture-host",
                "attempt_id": "attempt-1",
                "cpu": 1,
                "memory_mib": 16,
                "cpu_budget": 1,
                "memory_budget": 16
            }),
        )
        .unwrap();
        let id = started["id"].as_str().unwrap();
        let error = start(
            root.path(),
            &params(vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()], 1),
        )
        .unwrap_err();
        assert!(error.contains("local budget exceeded"), "{error}");
        let denied = cancel(root.path(), id, false).unwrap_err();
        assert!(denied.contains("cancel_execution"), "{denied}");
        assert_eq!(inspect(root.path(), id).unwrap()["status"], "running");
        cancel(root.path(), id, true).unwrap();
        thread::sleep(std::time::Duration::from_millis(200));
        let record = inspect(root.path(), id).unwrap();
        assert_eq!(record["status"], "cancelled");
        let pid = record["pid"].as_u64().unwrap() as u32;
        assert!(!pid_alive(pid));
        let listed = Command::new("ps")
            .args(["-o", "pid=", "-g", &pid.to_string()])
            .output()
            .unwrap();
        let remaining = String::from_utf8_lossy(&listed.stdout);
        assert!(
            remaining.trim().is_empty(),
            "process group still has children: {remaining}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_same_cargo_target_refuses_a_second_running_build() {
        let _durable = IsolatedDurableRuns::install();
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target_dir = target.path().display().to_string();
        let started = start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/sh", "-c", "sleep 30"],
                "cpu": 0,
                "target_dir": target_dir,
            }),
        )
        .unwrap();
        let id = started["id"].as_str().unwrap();
        let spelled = target
            .path()
            .join("..")
            .join(target.path().file_name().unwrap())
            .display()
            .to_string();
        let again = start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/sh", "-c", "sleep 30"],
                "cpu": 0,
                "target_dir": spelled,
            }),
        )
        .unwrap_err();
        assert!(
            again.contains("already has a running local build"),
            "{again}"
        );
        let independent = start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/sh", "-c", "sleep 30"],
                "cpu": 0,
                "target_dir": other.path().display().to_string(),
            }),
        )
        .unwrap();
        cancel(root.path(), id, false).unwrap();
        cancel(root.path(), independent["id"].as_str().unwrap(), false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_reused_pid_is_not_the_old_run() {
        let root = tempfile::tempdir().unwrap();
        let mut record = OwnedRun {
            id: "run-stale".into(),
            label: "stale".into(),
            status: "running".into(),
            kind: "local".into(),
            command: vec!["sleep".into()],
            remote: None,
            attempt_id: None,
            pid: Some(std::process::id()),
            start_time: Some(1),
            cpu: 1,
            memory_mib: 1,
            exit_code: None,
            log_path: String::new(),
            admission_id: String::new(),
            target_dir: String::new(),
            operation_id: String::new(),
            workspace_id: String::new(),
            events: Vec::new(),
        };
        push_event(&mut record, "started");
        store(root.path(), &record).unwrap();
        reconcile(root.path()).unwrap();
        let updated = inspect(root.path(), "run-stale").unwrap();
        assert_eq!(updated["status"], "lost");
        assert!(updated["exit_code"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_running_record_is_marked_lost_and_frees_the_budget() {
        let _durable = IsolatedDurableRuns::install();
        let root = tempfile::tempdir().unwrap();
        let blocked = start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/sh", "-c", "sleep 60"],
                "cpu": 1,
                "memory_mib": 16,
                "cpu_budget": 1,
                "memory_budget": 16
            }),
        )
        .unwrap();
        let id = blocked["id"].as_str().unwrap();
        cancel(root.path(), id, false).unwrap();
        let mut orphan = load(root.path(), id).unwrap();
        orphan.id = "run-orphan".into();
        orphan.status = "running".into();
        orphan.pid = Some(9_876_543);
        orphan.start_time = Some(42);
        orphan.exit_code = None;
        store(root.path(), &orphan).unwrap();
        reconcile(root.path()).unwrap();
        let lost = inspect(root.path(), "run-orphan").unwrap();
        assert_eq!(lost["status"], "lost");
        start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/echo", "after-lost"],
                "cpu": 1,
                "memory_mib": 16,
                "cpu_budget": 1,
                "memory_budget": 16
            }),
        )
        .expect("budget must return after reconcile marks the orphan lost");
    }

    #[cfg(unix)]
    #[test]
    fn start_fails_when_durable_run_sync_cannot_persist() {
        let blocking = tempfile::NamedTempFile::new().unwrap();
        crate::durable_run::set_test_durable_runs_dir(Some(blocking.path().to_path_buf()));
        let root = tempfile::tempdir().unwrap();
        let error = start(
            root.path(),
            &json!({
                "label": "local",
                "command": ["/bin/sh", "-c", "exit 0"],
                "cpu": 1,
                "memory_mib": 16,
                "cpu_budget": 2,
                "memory_budget": 32
            }),
        )
        .unwrap_err();
        crate::durable_run::set_test_durable_runs_dir(None);
        assert!(
            error.contains("durable run sync"),
            "expected fail-loud durable sync, got: {error}"
        );
        let listed = list(root.path()).unwrap();
        assert!(
            listed.as_array().unwrap().is_empty()
                || listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry["status"] != "running"),
            "start failure must not leave a running reservation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn probe_script_is_stored_from_the_child_environment() {
        let _durable = IsolatedDurableRuns::install();
        let root = tempfile::tempdir().unwrap();
        let started = start(
            root.path(),
            &json!({
                "label": "probe",
                "command": ["/bin/echo", "probe-child"],
                "cpu": 0,
                "probe_script": "printf '%s\\n' rustc_path=/usr/bin/rustc rustc_version=rustc-test cargo_path=/usr/bin/cargo cargo_version=cargo-test wrapper_path= linker_path= target_directory=/tmp/nxr-q-target cache_identity="
            }),
        )
        .unwrap();
        let id = started["id"].as_str().unwrap();
        let probe =
            std::fs::read_to_string(root.path().join("provenance").join(format!("{id}.probe")))
                .expect("child probe");
        assert!(probe.contains("rustc_path=/usr/bin/rustc"), "{probe}");
        assert!(
            probe.contains("target_directory=/tmp/nxr-q-target"),
            "{probe}"
        );
    }
}
