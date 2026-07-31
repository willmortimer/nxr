//! Persisted attach metadata for `nxr attach`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nxr_core::{RunSummary, RunTargetKind, list_runs};
use serde::{Deserialize, Serialize};

use super::state::{NodePhase, WatchState};

const SIDECAR_FILENAME: &str = "nxr-run.json";
const ATTACH_RUNS_DIR: &str = "attach-runs";
const SIDECAR_SCHEMA_VERSION: u32 = 1;
const HISTORY_RUN_PREFIX: &str = "hist-";

/// Persisted node snapshot for attach replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttachNodeRecord {
    pub id: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// On-disk attach session metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttachSession {
    pub schema_version: u32,
    pub run_id: String,
    pub recorded_at: u64,
    pub target_kind: RunTargetKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<PathBuf>,
    pub status: AttachRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<AttachNodeRecord>,
    /// Absolute path to the sidecar file (not serialized).
    #[serde(skip)]
    pub sidecar_path: PathBuf,
    /// True when this session was synthesized from run history (no TUI sidecar).
    #[serde(skip)]
    pub from_history: bool,
}

/// Whether the recorded run is still active.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachRunStatus {
    Running,
    Completed,
}

/// Errors resolving or reading attach sessions.
#[derive(Debug, thiserror::Error)]
pub enum AttachSessionError {
    #[error("no attachable runs found (run with --output tui, or check nxr history)")]
    NotFound,
    #[error("attach run not found: {run_id}")]
    UnknownRun { run_id: String },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid attach sidecar: {0}")]
    InvalidSidecar(String),
}

impl AttachSession {
    /// Write a new running session sidecar under XDG state and optional log-dir.
    ///
    /// # Errors
    ///
    /// Returns [`AttachSessionError`] when the sidecar cannot be written.
    pub fn write_running(
        run_id: &str,
        target_kind: RunTargetKind,
        target: &str,
        log_dir: Option<&Path>,
    ) -> Result<Self, AttachSessionError> {
        let session = Self {
            schema_version: SIDECAR_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            recorded_at: unix_now_secs(),
            target_kind,
            target: target.to_owned(),
            log_dir: log_dir.map(Path::to_path_buf),
            status: AttachRunStatus::Running,
            success: None,
            nodes: Vec::new(),
            sidecar_path: attach_runs_dir().join(format!("{run_id}.json")),
            from_history: false,
        };
        session.save()?;
        if let Some(dir) = log_dir {
            write_log_dir_sidecar(dir, &session)?;
        }
        Ok(session)
    }

    /// Update the session from live watch state and mark completion when requested.
    ///
    /// # Errors
    ///
    /// Returns [`AttachSessionError`] when the sidecar cannot be written.
    pub fn sync_from_watch(
        &mut self,
        state: &WatchState,
        completed: bool,
        success: Option<bool>,
    ) -> Result<(), AttachSessionError> {
        self.nodes = state
            .node_order
            .iter()
            .filter_map(|id| {
                state.nodes.get(id).map(|node| AttachNodeRecord {
                    id: id.clone(),
                    phase: node.phase.label().to_owned(),
                    duration_ms: node.duration_ms,
                })
            })
            .collect();
        if completed {
            self.status = AttachRunStatus::Completed;
            self.success = success;
        }
        self.save()?;
        if let Some(dir) = &self.log_dir {
            write_log_dir_sidecar(dir, self)?;
        }
        Ok(())
    }

    /// Build initial watch state for attach replay.
    #[must_use]
    pub fn to_watch_state(&self) -> WatchState {
        let mut state = WatchState {
            run_id: Some(self.run_id.clone()),
            root: self.target.clone(),
            run_complete: matches!(self.status, AttachRunStatus::Completed),
            success: self.success,
            diagnostic: if self.from_history {
                Some(
                    "history summary (no TUI sidecar / log-dir); re-run with --output tui for full attach"
                        .to_owned(),
                )
            } else {
                None
            },
            ..WatchState::default()
        };
        for node in &self.nodes {
            let phase = parse_phase_label(&node.phase);
            state.node_order.push(node.id.clone());
            state.nodes.insert(
                node.id.clone(),
                super::state::NodeState {
                    phase,
                    duration_ms: node.duration_ms,
                    ..super::state::NodeState::default()
                },
            );
        }
        state
    }

    fn save(&self) -> Result<(), AttachSessionError> {
        let dir = attach_runs_dir();
        fs::create_dir_all(&dir)?;
        let rendered = serde_json::to_string_pretty(self)
            .map_err(|error| AttachSessionError::InvalidSidecar(error.to_string()))?;
        fs::write(&self.sidecar_path, rendered)?;
        Ok(())
    }
}

/// List attachable sessions, newest first (TUI sidecars, then history fallbacks).
///
/// # Errors
///
/// Returns [`AttachSessionError`] when sidecars or history cannot be read.
pub fn list_attachable_runs() -> Result<Vec<AttachSession>, AttachSessionError> {
    let mut sessions = list_sidecar_sessions()?;
    let sidecar_ids: std::collections::BTreeSet<String> = sessions
        .iter()
        .map(|session| session.run_id.clone())
        .collect();

    for summary in list_runs()?.into_iter().rev() {
        let session = session_from_history(&summary);
        if sidecar_ids.contains(&session.run_id) {
            continue;
        }
        // Prefer sidecars that match the same target+timestamp window when present.
        if sessions.iter().any(|existing| {
            !existing.from_history
                && existing.target == session.target
                && existing.target_kind == session.target_kind
                && existing.recorded_at.abs_diff(session.recorded_at) <= 2
        }) {
            continue;
        }
        sessions.push(session);
    }

    sessions.sort_by_key(|session| std::cmp::Reverse(session.recorded_at));
    Ok(sessions)
}

fn list_sidecar_sessions() -> Result<Vec<AttachSession>, AttachSessionError> {
    let mut sessions = Vec::new();
    let dir = attach_runs_dir();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(mut session) = read_sidecar(&path) {
                session.sidecar_path = path;
                session.from_history = false;
                sessions.push(session);
            }
        }
    }
    Ok(sessions)
}

fn session_from_history(summary: &RunSummary) -> AttachSession {
    let run_id = history_run_id(summary);
    let success = summary.exit_code == 0;
    let phase = if success { "ok" } else { "failed" };
    AttachSession {
        schema_version: SIDECAR_SCHEMA_VERSION,
        run_id: run_id.clone(),
        recorded_at: summary.recorded_at,
        target_kind: summary.target_kind,
        target: summary.target.clone(),
        log_dir: None,
        status: AttachRunStatus::Completed,
        success: Some(success),
        nodes: vec![AttachNodeRecord {
            id: summary.target.clone(),
            phase: phase.to_owned(),
            duration_ms: Some(summary.duration_ms),
        }],
        sidecar_path: PathBuf::from(format!("history:{run_id}")),
        from_history: true,
    }
}

fn history_run_id(summary: &RunSummary) -> String {
    let safe_target = sanitize_node_name(&summary.target);
    format!("{HISTORY_RUN_PREFIX}{}-{safe_target}", summary.recorded_at)
}

/// Resolve a run id or the most recent attachable session.
///
/// Prefers TUI attach sidecars; falls back to [`nxr_core::list_runs`] history
/// summaries (synthetic `hist-<epoch>-<target>` ids) when no sidecar exists.
///
/// # Errors
///
/// Returns [`AttachSessionError`] when no matching session exists.
pub fn resolve_attach_run(run_id: Option<&str>) -> Result<AttachSession, AttachSessionError> {
    let sessions = list_attachable_runs()?;
    if sessions.is_empty() {
        return Err(AttachSessionError::NotFound);
    }
    if let Some(run_id) = run_id {
        sessions
            .into_iter()
            .find(|session| session.run_id == run_id)
            .ok_or_else(|| AttachSessionError::UnknownRun {
                run_id: run_id.to_owned(),
            })
    } else if let Some(sidecar) = sessions
        .iter()
        .find(|session| !session.from_history)
        .cloned()
    {
        Ok(sidecar)
    } else {
        sessions
            .into_iter()
            .next()
            .ok_or(AttachSessionError::NotFound)
    }
}

/// Load log tails from a session's log directory into `state`.
///
/// # Errors
///
/// Returns [`AttachSessionError`] on log-dir read failures.
pub fn hydrate_log_tails(
    session: &AttachSession,
    state: &mut WatchState,
) -> Result<(), AttachSessionError> {
    let Some(log_dir) = &session.log_dir else {
        return Ok(());
    };
    for node_id in &state.node_order {
        let safe = sanitize_node_name(node_id);
        let stdout_path = log_dir.join(format!("{safe}.stdout"));
        let stderr_path = log_dir.join(format!("{safe}.stderr"));
        if let Some(entry) = state.nodes.get_mut(node_id) {
            if stdout_path.is_file() {
                entry.stdout_tail = fs::read_to_string(stdout_path)?;
            }
            if stderr_path.is_file() {
                entry.stderr_tail = fs::read_to_string(stderr_path)?;
            }
        }
    }
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<AttachSession, AttachSessionError> {
    let contents = fs::read_to_string(path)?;
    let mut session: AttachSession = serde_json::from_str(&contents)
        .map_err(|error| AttachSessionError::InvalidSidecar(error.to_string()))?;
    if session.schema_version != SIDECAR_SCHEMA_VERSION {
        return Err(AttachSessionError::InvalidSidecar(format!(
            "unsupported schema version {}",
            session.schema_version
        )));
    }
    session.sidecar_path = path.to_path_buf();
    session.from_history = false;
    Ok(session)
}

fn write_log_dir_sidecar(
    log_dir: &Path,
    session: &AttachSession,
) -> Result<(), AttachSessionError> {
    fs::create_dir_all(log_dir)?;
    let path = log_dir.join(SIDECAR_FILENAME);
    let rendered = serde_json::to_string_pretty(session)
        .map_err(|error| AttachSessionError::InvalidSidecar(error.to_string()))?;
    fs::write(path, rendered)?;
    Ok(())
}

fn attach_runs_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_ATTACH_RUNS_DIR.with(|cell| cell.borrow().clone()) {
        return path;
    }

    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        return state_home.join("nxr").join(ATTACH_RUNS_DIR);
    }
    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .and_then(|dirs| dirs.state_dir().map(|path| path.join(ATTACH_RUNS_DIR)))
        .unwrap_or_else(|| PathBuf::from(".nxr").join(ATTACH_RUNS_DIR))
}

fn sanitize_node_name(node: &str) -> String {
    node.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '_',
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' => c,
            _ => '_',
        })
        .collect()
}

fn parse_phase_label(label: &str) -> NodePhase {
    match label {
        "running" => NodePhase::Running,
        "ok" => NodePhase::Succeeded,
        "failed" => NodePhase::Failed,
        "skipped" => NodePhase::Skipped,
        "cancelled" => NodePhase::Cancelled,
        "timed out" => NodePhase::TimedOut,
        _ => NodePhase::Queued,
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
    static TEST_ATTACH_RUNS_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_attach_runs_dir(path: Option<PathBuf>) {
    TEST_ATTACH_RUNS_DIR.with(|cell| *cell.borrow_mut() = path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_core::DiscoveryCacheOutcome;
    use tempfile::TempDir;

    #[test]
    fn write_and_resolve_most_recent() {
        let temp = TempDir::new().expect("tempdir");
        set_test_attach_runs_dir(Some(temp.path().to_path_buf()));

        let session = AttachSession::write_running("run-test1", RunTargetKind::Task, "ci", None)
            .expect("write");
        assert_eq!(session.run_id, "run-test1");

        let resolved = resolve_attach_run(None).expect("resolve");
        assert_eq!(resolved.run_id, "run-test1");
        assert!(!resolved.from_history);

        set_test_attach_runs_dir(None);
    }

    #[test]
    fn resolve_unknown_run_fails_closed() {
        let temp = TempDir::new().expect("tempdir");
        set_test_attach_runs_dir(Some(temp.path().to_path_buf()));
        AttachSession::write_running("run-a", RunTargetKind::Task, "ci", None).expect("write");

        let error = resolve_attach_run(Some("run-missing")).expect_err("missing");
        assert!(matches!(error, AttachSessionError::UnknownRun { .. }));

        set_test_attach_runs_dir(None);
    }

    #[test]
    fn history_fallback_builds_synthetic_session() {
        let summary = RunSummary {
            recorded_at: 1_700_000_000,
            target_kind: RunTargetKind::Task,
            target: "fmt-check".to_owned(),
            flake: Some(".".to_owned()),
            exit_code: 0,
            duration_ms: 42,
            discovery_cache: DiscoveryCacheOutcome::NotApplicable,
            discovery_miss_reasons: Vec::new(),
        };
        let session = session_from_history(&summary);
        assert!(session.from_history);
        assert_eq!(
            session.run_id,
            format!("{HISTORY_RUN_PREFIX}1700000000-fmt-check")
        );
        assert_eq!(session.target, "fmt-check");
        assert_eq!(session.nodes.len(), 1);
        assert_eq!(session.success, Some(true));
        assert!(session.to_watch_state().diagnostic.is_some());
    }

    #[test]
    fn sidecar_preferred_over_history_when_resolving_default() {
        let temp = TempDir::new().expect("tempdir");
        set_test_attach_runs_dir(Some(temp.path().to_path_buf()));
        AttachSession::write_running("run-sidecar", RunTargetKind::Task, "ci", None)
            .expect("write");

        let resolved = resolve_attach_run(None).expect("resolve");
        assert_eq!(resolved.run_id, "run-sidecar");
        assert!(!resolved.from_history);

        set_test_attach_runs_dir(None);
    }
}
