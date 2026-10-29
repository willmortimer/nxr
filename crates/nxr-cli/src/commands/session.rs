//! `nxr session` submits a run to the per-user supervisor.
//!
//! The CLI is not the parent of the child. `nix run` does not use this path
//! and still works when the supervisor is absent.

use std::path::Path;

use serde_json::{Value, json};

use nxr_core::{daemon_socket_path, try_connect};
use nxr_task::remote_execution::{ExecutionTransport, cancel_execution};

use crate::runner_output::RunnerOutput;

/// What `nxr session start` should run.
#[derive(Clone, Debug)]
pub struct SessionStart {
    pub label: String,
    pub command: Vec<String>,
    pub remote: Option<String>,
    pub attempt_id: Option<String>,
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

/// Submit `request` to the supervisor on `socket`.
///
/// # Errors
///
/// Returns an error when the supervisor is down, the command is missing, or
/// the budget refuses the run. This function does not spawn the child.
pub fn start_on(
    socket: &Path,
    request: &SessionStart,
    budget: LocalBudget,
    cache_env: &std::collections::BTreeMap<String, String>,
    runner: RunnerOutput,
) -> Result<(), String> {
    let mode = nxr_task::compiler_cache::compiler_cache_mode(cache_env);
    let assignments: std::collections::BTreeMap<String, String> =
        nxr_task::compiler_cache::compiler_cache_assignments(cache_env)
            .into_iter()
            .collect();
    let value = call(
        socket,
        "run.start",
        json!({
            "label": request.label,
            "command": request.command,
            "remote": request.remote,
            "attempt_id": request.attempt_id,
            "cpu": request.cpu,
            "memory_mib": request.memory_mib,
            "cpu_budget": budget.cpu,
            "memory_budget": budget.memory_mib,
            "cache_mode": mode,
            "cache_env": assignments,
        }),
    )?;
    let id = value["id"].as_str().unwrap_or("session");
    runner.info(id).map_err(|err| err.to_string())
}

/// Submit using the default supervisor socket.
///
/// # Errors
///
/// Returns an error when the supervisor is down or the run is refused.
pub fn start(
    request: &SessionStart,
    budget: LocalBudget,
    runner: RunnerOutput,
) -> Result<(), String> {
    let socket = daemon_socket_path();
    start_on(
        &socket,
        request,
        budget,
        &nxr_task::compiler_cache::compiler_cache_from_process(),
        runner,
    )
}

/// Print stored runs.
///
/// # Errors
///
/// Returns an error when the supervisor is down.
pub fn list(runner: RunnerOutput) -> Result<(), String> {
    let socket = daemon_socket_path();
    let value = call(&socket, "run.list", json!({}))?;
    runner
        .info(serde_json::to_string(&value).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())
}

/// Print events after `after`.
///
/// # Errors
///
/// Returns an error when the supervisor is down or the run is missing.
pub fn events(id: &str, after: u64, runner: RunnerOutput) -> Result<(), String> {
    let socket = daemon_socket_path();
    let value = call(&socket, "run.events", json!({ "id": id, "after": after }))?;
    runner
        .info(serde_json::to_string(&value).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())
}

/// Print one run.
///
/// # Errors
///
/// Returns an error when the supervisor is down or the run is missing.
pub fn inspect(id: &str, runner: RunnerOutput) -> Result<(), String> {
    let socket = daemon_socket_path();
    let value = call(&socket, "run.inspect", json!({ "id": id }))?;
    runner
        .info(serde_json::to_string(&value).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())
}

/// Cancel a run. A stored attempt id is cancelled on `transport` before the
/// local process group is signalled.
///
/// # Errors
///
/// Returns an error when the supervisor is down, the remote cancel fails, or
/// the local stop fails. Stopping only the local client is not success for a
/// remote submit.
pub fn cancel_with<T: ExecutionTransport>(
    socket: &Path,
    id: &str,
    transport: Option<&T>,
    runner: RunnerOutput,
) -> Result<(), String> {
    let record = call(socket, "run.inspect", json!({ "id": id }))?;
    let confirmed = if let Some(attempt) = record["attempt_id"].as_str() {
        let transport = transport.ok_or_else(|| {
            "remote cancel needs an execution transport; killing the local client is not enough"
                .to_owned()
        })?;
        cancel_execution(transport, attempt)?;
        true
    } else if record["kind"] == "remote" {
        return Err("remote run has no stored attempt id".into());
    } else {
        false
    };
    let value = call(
        socket,
        "run.cancel",
        json!({ "id": id, "remote_confirmed": confirmed }),
    )?;
    runner
        .info(value["status"].as_str().unwrap_or("cancelled"))
        .map_err(|err| err.to_string())
}

/// Cancel using the default socket and `NXR_EXECUTION_URL` when the run has an attempt id.
///
/// # Errors
///
/// Returns an error when the supervisor is down or a remote cancel cannot be completed.
pub fn cancel(id: &str, runner: RunnerOutput) -> Result<(), String> {
    let socket = daemon_socket_path();
    let record = call(&socket, "run.inspect", json!({ "id": id }))?;
    if record["attempt_id"].as_str().is_some() {
        let url = std::env::var("NXR_EXECUTION_URL").map_err(|_| {
            "remote cancel needs NXR_EXECUTION_URL; killing the local client is not enough"
                .to_owned()
        })?;
        let transport = CurlTransport { url };
        cancel_with(&socket, id, Some(&transport), runner)
    } else {
        cancel_with::<CurlTransport>(&socket, id, None, runner)
    }
}

struct CurlTransport {
    url: String,
}

impl ExecutionTransport for CurlTransport {
    fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
        let mut payload = body.clone();
        if let Some(object) = payload.as_object_mut() {
            object.insert("method".into(), json!(method));
        }
        let output = std::process::Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "-X",
                "POST",
                "-H",
                "content-type: application/json",
                "--data",
                &payload.to_string(),
                &self.url,
            ])
            .output()
            .map_err(|err| format!("execution transport: {err}"))?;
        if !output.status.success() {
            return Err("execution transport rejected the cancel".into());
        }
        serde_json::from_slice(&output.stdout).map_err(|err| format!("execution transport: {err}"))
    }
}

fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let mut connection = try_connect(socket).map_err(|_| {
        "execution supervisor is not running; nxr session start does not own the child".to_owned()
    })?;
    connection
        .call(method, Some(params))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use nxr_core::{DaemonState, serve};
    use nxr_task::remote_execution::ExecutionTransport;
    use serde_json::{Value, json};

    use super::*;

    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl ExecutionTransport for Fake {
        fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((method.to_owned(), body.clone()));
            Ok(json!({ "cancelled": true }))
        }
    }

    fn quiet() -> RunnerOutput {
        use crate::output_options::{ColorWhen, LogFormat, OutputOptions};
        RunnerOutput::new(OutputOptions::new(
            1,
            0,
            false,
            true,
            ColorWhen::Never,
            LogFormat::Human,
        ))
    }

    fn boot() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nxrd.sock");
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let serve_socket = socket.clone();
        thread::spawn(move || {
            let _ = serve(&serve_socket, state);
        });
        for _ in 0..50 {
            if try_connect(&socket).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        (dir, socket)
    }

    #[test]
    fn session_start_fails_closed_when_the_supervisor_is_down() {
        let dir = tempfile::tempdir().unwrap();
        let error = start_on(
            &dir.path().join("missing.sock"),
            &SessionStart {
                label: "local".into(),
                command: vec!["/bin/echo".into(), "hi".into()],
                remote: None,
                attempt_id: None,
                cpu: 0,
                memory_mib: 0,
            },
            LocalBudget {
                cpu: 1,
                memory_mib: 1,
            },
            &std::collections::BTreeMap::new(),
            quiet(),
        )
        .unwrap_err();
        assert!(error.contains("not running"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_client_sees_the_exit_code_after_the_starter_returns() {
        let (_dir, socket) = boot();
        start_on(
            &socket,
            &SessionStart {
                label: "local".into(),
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo from-supervisor; exit 3".into(),
                ],
                remote: None,
                attempt_id: None,
                cpu: 1,
                memory_mib: 8,
            },
            LocalBudget {
                cpu: 2,
                memory_mib: 32,
            },
            &std::collections::BTreeMap::new(),
            quiet(),
        )
        .unwrap();
        let listed = call(&socket, "run.list", json!({})).unwrap();
        let id = listed[0]["id"].as_str().unwrap().to_owned();
        let mut record = call(&socket, "run.inspect", json!({ "id": id })).unwrap();
        for _ in 0..50 {
            if record["status"] == "exited" {
                break;
            }
            thread::sleep(Duration::from_millis(20));
            record = call(&socket, "run.inspect", json!({ "id": id })).unwrap();
        }
        assert_eq!(record["exit_code"], 3);
        let text = call(&socket, "run.logs", json!({ "id": id })).unwrap();
        assert!(text["text"].as_str().unwrap().contains("from-supervisor"));
        let transport = Fake {
            calls: Mutex::new(Vec::new()),
        };
        start_on(
            &socket,
            &SessionStart {
                label: "remote".into(),
                command: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
                remote: Some("fixture-host".into()),
                attempt_id: Some("attempt-9".into()),
                cpu: 1,
                memory_mib: 8,
            },
            LocalBudget {
                cpu: 2,
                memory_mib: 64,
            },
            &std::collections::BTreeMap::new(),
            quiet(),
        )
        .unwrap();
        let again = call(&socket, "run.list", json!({})).unwrap();
        let remote_id = again
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["attempt_id"] == "attempt-9")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let denied = cancel_with::<Fake>(&socket, &remote_id, None, quiet()).unwrap_err();
        assert!(denied.contains("not enough"), "{denied}");
        cancel_with(&socket, &remote_id, Some(&transport), quiet()).unwrap();
        let calls = transport.calls.lock().expect("calls");
        assert_eq!(calls[0].0, "cancel");
        assert_eq!(calls[0].1["attempt_id"], "attempt-9");
        thread::sleep(Duration::from_millis(200));
        let stopped = call(&socket, "run.inspect", json!({ "id": remote_id })).unwrap();
        assert_eq!(stopped["status"], "cancelled");
        let pid = stopped["pid"].as_u64().unwrap();
        let listed = std::process::Command::new("ps")
            .args(["-o", "pid=", "-g", &pid.to_string()])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&listed.stdout).trim().is_empty());
        let _ = call(&socket, "shutdown", json!({}));
    }

    fn nxr_bin() -> std::path::PathBuf {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_nxr") {
            return std::path::PathBuf::from(path);
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/nxr")
    }

    #[cfg(unix)]
    #[test]
    fn starter_cli_exit_leaves_the_run_for_a_new_process() {
        use std::process::{Command, Stdio};

        let (_dir, socket) = boot();
        let nxr = nxr_bin();
        assert!(
            nxr.is_file(),
            "nxr binary missing at {}; run cargo build -p nxr-cli first",
            nxr.display()
        );
        let mut starter = Command::new(&nxr)
            .env("NXR_DAEMON_SOCKET", &socket)
            .env("NXR_LOCAL_CPU", "2")
            .env("NXR_LOCAL_MEMORY_MIB", "64")
            .args([
                "session",
                "start",
                "wave5",
                "--cpu",
                "1",
                "--memory-mib",
                "8",
                "--",
                "/bin/sh",
                "-c",
                "echo wave5-alive; sleep 30; exit 7",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start cli");
        let starter_pid = starter.id();
        // Wait until the supervisor has the run before killing the CLI. A
        // 200ms race on a cold binary can kill start before run.start lands.
        let mut id = None;
        for _ in 0..100 {
            if let Ok(listed) = call(&socket, "run.list", json!({}))
                && let Some(first) = listed.as_array().and_then(|runs| runs.first())
                && let Some(run_id) = first["id"].as_str()
            {
                id = Some(run_id.to_owned());
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = starter.kill();
        let waited = starter.wait_with_output().expect("starter wait");
        assert!(
            Command::new("ps")
                .args(["-p", &starter_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()
                .is_none_or(|status| !status.success()),
            "the initiating CLI must be gone"
        );
        let id = id.unwrap_or_else(|| {
            panic!(
                "supervisor never stored the run; starter status={} stdout={} stderr={}",
                waited.status,
                String::from_utf8_lossy(&waited.stdout),
                String::from_utf8_lossy(&waited.stderr)
            )
        });
        let inspected = Command::new(&nxr)
            .env("NXR_DAEMON_SOCKET", &socket)
            .args(["session", "inspect", &id])
            .output()
            .expect("inspect");
        // RunnerOutput::info writes the JSON record to stderr, not stdout.
        let inspect_text = format!(
            "{}{}",
            String::from_utf8_lossy(&inspected.stdout),
            String::from_utf8_lossy(&inspected.stderr)
        );
        assert!(inspected.status.success(), "inspect failed: {inspect_text}");
        assert!(
            inspect_text.contains("running")
                || inspect_text.contains("exited")
                || inspect_text.contains(&id),
            "{inspect_text}"
        );
        let record = call(&socket, "run.inspect", json!({ "id": id })).unwrap();
        assert!(record.get("pid").and_then(|v| v.as_u64()).is_some());
        assert!(record.get("start_time").is_some());
        let refused = Command::new(&nxr)
            .env("NXR_DAEMON_SOCKET", &socket)
            .env("NXR_LOCAL_CPU", "1")
            .env("NXR_LOCAL_MEMORY_MIB", "8")
            .args([
                "session",
                "start",
                "wave5-refuse",
                "--cpu",
                "1",
                "--memory-mib",
                "8",
                "--",
                "/bin/sleep",
                "5",
            ])
            .output()
            .expect("second start");
        assert!(
            !refused.status.success(),
            "budget 1 must refuse a second start while the first still holds the reservation"
        );
        let cancel = Command::new(&nxr)
            .env("NXR_DAEMON_SOCKET", &socket)
            .args(["session", "cancel", &id])
            .output()
            .expect("cancel");
        assert!(
            cancel.status.success(),
            "{}",
            String::from_utf8_lossy(&cancel.stderr)
        );
        thread::sleep(Duration::from_millis(200));
        let stopped = call(&socket, "run.inspect", json!({ "id": id })).unwrap();
        assert_eq!(stopped["status"], "cancelled");
        let pid = stopped["pid"].as_u64().unwrap();
        let orphans = Command::new("ps")
            .args(["-o", "pid=", "-g", &pid.to_string()])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&orphans.stdout).trim().is_empty());
        let _ = call(&socket, "shutdown", json!({}));
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_starts_serialize_on_the_admission_lock() {
        use std::process::{Command, Stdio};

        let (_dir, socket) = boot();
        let nxr = nxr_bin();
        let spawn = || {
            Command::new(&nxr)
                .env("NXR_DAEMON_SOCKET", &socket)
                .env("NXR_LOCAL_CPU", "1")
                .env("NXR_LOCAL_MEMORY_MIB", "8")
                .args([
                    "session",
                    "start",
                    "race",
                    "--cpu",
                    "1",
                    "--memory-mib",
                    "8",
                    "--",
                    "/bin/sh",
                    "-c",
                    "sleep 20",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("race start")
        };
        let mut first = spawn();
        let mut second = spawn();
        let first_status = first.wait().expect("first");
        let second_status = second.wait().expect("second");
        assert!(
            first_status.success() ^ second_status.success(),
            "exactly one concurrent start may pass a budget of 1"
        );
        let listed = call(&socket, "run.list", json!({})).unwrap();
        let running = listed
            .as_array()
            .unwrap()
            .iter()
            .filter(|run| run["status"] == "running")
            .count();
        assert_eq!(running, 1);
        let id = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["status"] == "running")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        call(&socket, "run.cancel", json!({ "id": id })).unwrap();
        let _ = call(&socket, "shutdown", json!({}));
    }
}
