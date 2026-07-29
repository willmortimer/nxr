//! `nxr daemon` — optional local cache/coordination daemon (`nxrd`).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nxr_core::diagnostics::exit;
use nxr_core::{
    DAEMON_PROTOCOL_VERSION, DaemonClientError, DaemonState, DaemonStatus, cleanup_socket_files,
    daemon_connect_enabled, daemon_socket_path, read_pid_file, serve, try_connect,
};
use serde::Serialize;

use crate::runner_output::RunnerOutput;

/// Errors while managing the daemon lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum DaemonCommandError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Client(#[from] DaemonClientError),
    #[error("daemon connect disabled (NXR_DAEMON=off)")]
    Disabled,
    #[error("failed to spawn daemon process")]
    SpawnFailed,
    #[error("daemon did not become ready at {socket}")]
    NotReady { socket: String },
}

impl DaemonCommandError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Disabled | Self::Client(DaemonClientError::Disabled) => exit::USAGE,
            Self::Client(DaemonClientError::Absent) | Self::NotReady { .. } => exit::NOT_FOUND,
            Self::Client(DaemonClientError::ProtocolMismatch(_)) => exit::INVALID_METADATA,
            Self::Io(_) | Self::Json(_) | Self::SpawnFailed | Self::Client(_) => exit::EVALUATION,
        }
    }
}

#[derive(Serialize)]
struct AbsentStatus {
    running: bool,
    socket: String,
    connect_enabled: bool,
    protocol_version: u32,
}

#[derive(Serialize)]
struct RunningStatus {
    running: bool,
    #[serde(flatten)]
    status: DaemonStatus,
    connect_enabled: bool,
}

/// Run the daemon serve loop on `socket` (blocks until shutdown).
///
/// # Errors
///
/// Returns [`DaemonCommandError`] when the listener cannot start.
pub fn serve_foreground(socket: Option<PathBuf>) -> Result<(), DaemonCommandError> {
    let socket = socket.unwrap_or_else(daemon_socket_path);
    let state = Arc::new(Mutex::new(DaemonState::new()));
    serve(&socket, state)?;
    Ok(())
}

/// Start the daemon in the background (or foreground when requested).
///
/// # Errors
///
/// Returns [`DaemonCommandError`] when spawn or readiness fails.
pub fn start(
    socket: Option<PathBuf>,
    foreground: bool,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DaemonCommandError> {
    if !daemon_connect_enabled() {
        return Err(DaemonCommandError::Disabled);
    }
    let socket = socket.unwrap_or_else(daemon_socket_path);

    if foreground {
        runner.info(format!(
            "nxrd listening on {} (protocol v{DAEMON_PROTOCOL_VERSION}, role=cache)",
            socket.display()
        ))?;
        serve_foreground(Some(socket))?;
        return Ok(exit::SUCCESS);
    }

    // Already running?
    if try_connect(&socket).is_ok() {
        let mut conn = try_connect(&socket)?;
        let status: DaemonStatus = conn.call("status", None)?;
        write_status(
            json,
            &RunningStatus {
                running: true,
                status,
                connect_enabled: true,
            },
        )?;
        return Ok(exit::SUCCESS);
    }

    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("daemon")
        .arg("start")
        .arg("--foreground")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| DaemonCommandError::SpawnFailed)?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if try_connect(&socket).is_ok() {
            let mut conn = try_connect(&socket)?;
            let status: DaemonStatus = conn.call("status", None)?;
            // Detach: do not wait on child.
            let _ = child;
            write_status(
                json,
                &RunningStatus {
                    running: true,
                    status,
                    connect_enabled: true,
                },
            )?;
            return Ok(exit::SUCCESS);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(DaemonCommandError::NotReady {
        socket: socket.display().to_string(),
    })
}

/// Stop a running daemon via the shutdown method (fallback: PID signal).
///
/// # Errors
///
/// Returns [`DaemonCommandError`] when stop fails unexpectedly.
pub fn stop(
    socket: Option<PathBuf>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DaemonCommandError> {
    let socket = socket.unwrap_or_else(daemon_socket_path);
    match try_connect(&socket) {
        Ok(mut conn) => {
            let _: serde_json::Value = conn.call("shutdown", None)?;
            // Give the process a moment to exit and unlink.
            thread::sleep(Duration::from_millis(100));
            if json {
                writeln!(
                    io::stdout(),
                    "{}",
                    serde_json::json!({ "stopped": true, "socket": socket.display().to_string() })
                )?;
            } else {
                runner.info(format!("stopped daemon at {}", socket.display()))?;
            }
            Ok(exit::SUCCESS)
        }
        Err(DaemonClientError::Absent) | Err(DaemonClientError::Disabled) => {
            if let Some(pid) = read_pid_file(&socket) {
                #[cfg(unix)]
                {
                    let _ = Command::new("kill").arg(pid.to_string()).status();
                }
                cleanup_socket_files(&socket);
            }
            if json {
                writeln!(
                    io::stdout(),
                    "{}",
                    serde_json::json!({ "stopped": false, "already_absent": true, "socket": socket.display().to_string() })
                )?;
            } else {
                runner.info("daemon not running")?;
            }
            Ok(exit::SUCCESS)
        }
        Err(error) => Err(error.into()),
    }
}

/// Print daemon status (absent is success with `running: false`).
///
/// # Errors
///
/// Returns [`DaemonCommandError`] on I/O failures.
pub fn status(
    socket: Option<PathBuf>,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, DaemonCommandError> {
    let socket = socket.unwrap_or_else(daemon_socket_path);
    let connect_enabled = daemon_connect_enabled();
    if !connect_enabled {
        write_status(
            json,
            &AbsentStatus {
                running: false,
                socket: socket.display().to_string(),
                connect_enabled: false,
                protocol_version: DAEMON_PROTOCOL_VERSION,
            },
        )?;
        if !json {
            runner.info("daemon connect disabled (NXR_DAEMON=off)")?;
        }
        return Ok(exit::SUCCESS);
    }

    match try_connect(&socket) {
        Ok(mut conn) => {
            let status: DaemonStatus = conn.call("status", None)?;
            if json {
                write_status(
                    json,
                    &RunningStatus {
                        running: true,
                        status,
                        connect_enabled: true,
                    },
                )?;
            } else {
                runner.info(format!(
                    "running pid={} protocol={} role={} socket={} discovery={} plans={} dev_env={}",
                    status.pid,
                    status.protocol_version,
                    status.role,
                    status.socket,
                    status.discovery_entries,
                    status.plan_entries,
                    status.dev_env_entries
                ))?;
            }
            Ok(exit::SUCCESS)
        }
        Err(DaemonClientError::Absent) | Err(DaemonClientError::ProtocolMismatch(_)) => {
            write_status(
                json,
                &AbsentStatus {
                    running: false,
                    socket: socket.display().to_string(),
                    connect_enabled: true,
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                },
            )?;
            if !json {
                runner.info(format!("daemon not running (socket {})", socket.display()))?;
            }
            Ok(exit::SUCCESS)
        }
        Err(DaemonClientError::Disabled) => {
            write_status(
                json,
                &AbsentStatus {
                    running: false,
                    socket: socket.display().to_string(),
                    connect_enabled: false,
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                },
            )?;
            Ok(exit::SUCCESS)
        }
        Err(error) => Err(error.into()),
    }
}

fn write_status(json: bool, value: &impl Serialize) -> Result<(), DaemonCommandError> {
    if json {
        writeln!(io::stdout(), "{}", serde_json::to_string(value)?)?;
    }
    Ok(())
}
