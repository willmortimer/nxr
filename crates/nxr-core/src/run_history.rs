//! Persisted summaries of recent nxr runs under the user state directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Environment variable overriding how many run summaries are retained.
pub const RUN_HISTORY_LIMIT_ENV: &str = "NXR_RUN_HISTORY_LIMIT";

/// Default number of run summaries kept on disk.
pub const DEFAULT_RUN_HISTORY_LIMIT: usize = 50;

const RUN_HISTORY_SCHEMA_VERSION: u32 = 1;
const RUN_HISTORY_FILENAME: &str = "run-history.json";

/// Whether discovery cache was used for the invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCacheOutcome {
    Hit,
    Miss,
    NotApplicable,
}

/// App vs task execution target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTargetKind {
    App,
    Task,
}

/// One persisted run summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub recorded_at: u64,
    pub target_kind: RunTargetKind,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flake: Option<String>,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub discovery_cache: DiscoveryCacheOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovery_miss_reasons: Vec<String>,
}

/// Inputs for appending a run summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummaryInput {
    pub target_kind: RunTargetKind,
    pub target: String,
    pub flake: Option<String>,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub discovery_cache: DiscoveryCacheOutcome,
    pub discovery_miss_reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct RunHistoryFile {
    schema_version: u32,
    entries: Vec<RunSummary>,
}

/// Append a run summary and trim to the configured retention limit.
///
/// Write failures are ignored so execution results are never blocked by history I/O.
pub fn record_run(input: RunSummaryInput) {
    let _ = record_run_result(input);
}

/// Like [`record_run`] but surfaces I/O errors to callers (tests, diagnostics).
///
/// # Errors
///
/// Returns [`io::Error`] when the history file cannot be read or written.
pub fn record_run_result(input: RunSummaryInput) -> io::Result<()> {
    let Some(path) = run_history_path() else {
        return Ok(());
    };
    let mut file = RunHistoryFile::load_from(&path)?;
    file.entries.push(RunSummary {
        recorded_at: unix_now_secs(),
        target_kind: input.target_kind,
        target: input.target,
        flake: input.flake,
        exit_code: input.exit_code,
        duration_ms: input.duration_ms,
        discovery_cache: input.discovery_cache,
        discovery_miss_reasons: input.discovery_miss_reasons,
    });
    let limit = run_history_limit();
    if file.entries.len() > limit {
        let drop = file.entries.len() - limit;
        file.entries.drain(0..drop);
    }
    file.save_to(&path)
}

/// Return the most recent run summaries, newest last.
///
/// # Errors
///
/// Returns [`io::Error`] when the history file cannot be read.
pub fn list_runs() -> io::Result<Vec<RunSummary>> {
    let Some(path) = run_history_path() else {
        return Ok(Vec::new());
    };
    Ok(RunHistoryFile::load_from(&path)?.entries)
}

/// Remove all persisted run summaries.
///
/// # Errors
///
/// Returns [`io::Error`] when the history file cannot be removed.
pub fn clear_runs() -> io::Result<()> {
    let Some(path) = run_history_path() else {
        return Ok(());
    };
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

impl RunHistoryFile {
    fn empty() -> Self {
        Self {
            schema_version: RUN_HISTORY_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    fn load_from(path: &Path) -> io::Result<Self> {
        if !path.is_file() {
            return Ok(Self::empty());
        }
        let contents = fs::read_to_string(path)?;
        let file: Self = serde_json::from_str(&contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if file.schema_version != RUN_HISTORY_SCHEMA_VERSION {
            return Ok(Self::empty());
        }
        Ok(file)
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let rendered = serde_json::to_string_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, rendered)
    }
}

fn run_history_path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = TEST_RUN_HISTORY_PATH.with(|cell| cell.borrow().clone()) {
        return Some(path);
    }

    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        return Some(state_home.join("nxr").join(RUN_HISTORY_FILENAME));
    }

    directories::ProjectDirs::from("dev", "nxr", "nxr").and_then(|dirs| {
        dirs.state_dir()
            .map(|path| path.join(RUN_HISTORY_FILENAME))
    })
}

fn run_history_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = TEST_RUN_HISTORY_LIMIT.with(|cell| *cell.borrow()) {
        return limit;
    }

    match std::env::var(RUN_HISTORY_LIMIT_ENV) {
        Ok(raw) => raw.parse::<usize>().unwrap_or(DEFAULT_RUN_HISTORY_LIMIT),
        Err(_) => DEFAULT_RUN_HISTORY_LIMIT,
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
thread_local! {
    static TEST_RUN_HISTORY_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static TEST_RUN_HISTORY_LIMIT: std::cell::RefCell<Option<usize>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_run_history_path(path: Option<PathBuf>) {
    TEST_RUN_HISTORY_PATH.with(|cell| *cell.borrow_mut() = path);
}

#[cfg(test)]
pub(crate) fn set_test_run_history_limit(limit: Option<usize>) {
    TEST_RUN_HISTORY_LIMIT.with(|cell| *cell.borrow_mut() = limit);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        DiscoveryCacheOutcome, RunSummaryInput, RunTargetKind, clear_runs, list_runs, record_run,
        record_run_result, set_test_run_history_limit, set_test_run_history_path,
    };
    use tempfile::TempDir;

    #[test]
    fn record_and_list_trim_to_limit() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("run-history.json");
        set_test_run_history_path(Some(path));
        set_test_run_history_limit(Some(2));
        clear_runs().expect("clear");

        for index in 0..3 {
            record_run_result(RunSummaryInput {
                target_kind: RunTargetKind::Task,
                target: format!("task-{index}"),
                flake: Some(".".to_owned()),
                exit_code: 0,
                duration_ms: 10,
                discovery_cache: DiscoveryCacheOutcome::Hit,
                discovery_miss_reasons: Vec::new(),
            })
            .expect("record");
        }

        let runs = list_runs().expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].target, "task-1");
        assert_eq!(runs[1].target, "task-2");

        set_test_run_history_path(None);
        set_test_run_history_limit(None);
    }

    #[test]
    fn record_run_ignores_io_errors() {
        set_test_run_history_path(Some(PathBuf::from("/nonexistent/nxr/run-history.json")));
        record_run(RunSummaryInput {
            target_kind: RunTargetKind::App,
            target: "hello".to_owned(),
            flake: None,
            exit_code: 0,
            duration_ms: 1,
            discovery_cache: DiscoveryCacheOutcome::NotApplicable,
            discovery_miss_reasons: Vec::new(),
        });
        set_test_run_history_path(None);
    }
}
