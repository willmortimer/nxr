//! Detached run records. These are nxr sessions, not fleet Profile Assignments.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::runner_output::RunnerOutput;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionRecord {
    id: String,
    label: String,
    status: String,
    events: Vec<SessionEvent>,
    artifacts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionEvent {
    cursor: u64,
    message: String,
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

fn session_path(id: &str) -> Result<PathBuf, String> {
    if id.trim().is_empty() || id.contains('/') || id.contains("..") {
        return Err("run id is not a session name".into());
    }
    Ok(session_dir()?.join(format!("{id}.json")))
}

fn load(id: &str) -> Result<SessionRecord, String> {
    let text =
        fs::read_to_string(session_path(id)?).map_err(|err| format!("session {id}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("session json: {err}"))
}

fn store(record: &SessionRecord) -> Result<(), String> {
    let body = serde_json::to_string_pretty(record).map_err(|err| err.to_string())?;
    fs::write(session_path(&record.id)?, body).map_err(|err| err.to_string())
}

pub fn start(label: &str, runner: RunnerOutput) -> Result<(), String> {
    let id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let record = SessionRecord {
        id: id.clone(),
        label: label.to_owned(),
        status: "running".into(),
        events: vec![SessionEvent {
            cursor: 1,
            message: "detached".into(),
        }],
        artifacts: Vec::new(),
    };
    store(&record)?;
    runner.info(id).map_err(|err| err.to_string())
}

pub fn events(id: &str, after: u64, runner: RunnerOutput) -> Result<(), String> {
    let record = load(id)?;
    for event in record.events.iter().filter(|event| event.cursor > after) {
        runner
            .info(format!("{} {}", event.cursor, event.message))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub fn inspect(id: &str, runner: RunnerOutput) -> Result<(), String> {
    let record = load(id)?;
    runner
        .info(format!(
            "{} {} artifacts {}",
            record.status,
            record.label,
            if record.artifacts.is_empty() {
                "none".into()
            } else {
                record.artifacts.join(",")
            }
        ))
        .map_err(|err| err.to_string())
}

pub fn cancel(id: &str, runner: RunnerOutput) -> Result<(), String> {
    let mut record = load(id)?;
    record.status = "cancelled".into();
    let cursor = record.events.last().map(|event| event.cursor).unwrap_or(0) + 1;
    record.events.push(SessionEvent {
        cursor,
        message: "cancelled".into(),
    });
    store(&record)?;
    runner
        .info(format!("{id} cancelled"))
        .map_err(|err| err.to_string())
}
