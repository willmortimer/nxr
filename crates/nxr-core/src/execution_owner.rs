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
    events: Vec<RunEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunEvent {
    cursor: u64,
    message: String,
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
    with_lock(dir, || {
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
                events: Vec::new(),
            };
            push_event(&mut refused, "refused");
            store(dir, &refused)?;
            return Err("local budget exceeded".into());
        }
        let log_path = dir.join(format!("{id}.log"));
        let log = File::create(&log_path).map_err(|err| err.to_string())?;
        let stderr = log.try_clone().map_err(|err| err.to_string())?;
        let mut child = Command::new(&command[0]);
        child.args(&command[1..]);
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
        let mut spawned = child
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", command[0]))?;
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
            events: Vec::new(),
        };
        push_event(&mut record, "started");
        if record.kind == "remote" {
            push_event(&mut record, "submitted");
        }
        if cache_mode != "off" {
            push_event(&mut record, &format!("compiler-cache {cache_mode}"));
        }
        store(dir, &record)?;
        let command_for_durable = record.command.clone();
        durable_run::sync_supervisor_started(&id, &command_for_durable).map_err(|err| {
            let _ = stop_group(pid);
            let _ = fs::remove_file(dir.join(format!("{id}.json")));
            let _ = fs::remove_file(&log_path);
            durable_run::remove_durable_run_artifacts(&id);
            format!("durable run sync: {err}")
        })?;
        let wait_dir = dir.to_path_buf();
        let wait_id = id.clone();
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
                Some(pid) if pid_alive(pid) => match record.start_time {
                    Some(expected) => capture_start_time(pid) == Some(expected),
                    None => true,
                },
                _ => false,
            };
            if !still {
                record.status = "lost".into();
                record.exit_code = None;
                push_event(&mut record, "lost");
                store(dir, &record)?;
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
}
