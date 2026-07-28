//! `nxr history` — recent run summaries persisted under XDG state.

use std::io::{self, Write};
use std::time::Instant;

use nxr_completion::{
    DiscoveryCacheMissReason, DiscoveryCacheOptions, DiscoveryContext,
    discovery_cache_entry_with_options,
};
use nxr_core::diagnostics::exit;
use nxr_core::{
    DiscoveryCacheOutcome, RunSummary, RunSummaryInput, RunTargetKind, clear_runs, list_runs,
    record_run,
};
use serde::Serialize;

use crate::runner_output::RunnerOutput;

/// Errors while reading or clearing run history.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl HistoryError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

#[derive(Serialize)]
struct HistoryEnvelope {
    schema_version: u32,
    entries: Vec<RunSummary>,
}

/// Print persisted run summaries.
///
/// # Errors
///
/// Returns [`HistoryError`] when history cannot be read or rendered.
pub fn list(json: bool, runner: RunnerOutput) -> Result<(), HistoryError> {
    let entries = list_runs()?;
    if json {
        let payload = HistoryEnvelope {
            schema_version: 1,
            entries,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
        return Ok(());
    }

    if entries.is_empty() {
        runner
            .info("no run history entries")
            .map_err(HistoryError::Io)?;
        return Ok(());
    }

    for entry in entries {
        let flake = entry.flake.as_deref().unwrap_or("-");
        runner
            .info(format!(
                "{} {} exit={} duration_ms={} discovery_cache={:?} flake={flake}",
                format_target_kind(entry.target_kind),
                entry.target,
                entry.exit_code,
                entry.duration_ms,
                entry.discovery_cache,
            ))
            .map_err(HistoryError::Io)?;
    }
    Ok(())
}

/// Remove all persisted run summaries.
///
/// # Errors
///
/// Returns [`HistoryError`] when history cannot be cleared.
pub fn clear(json: bool, runner: RunnerOutput) -> Result<(), HistoryError> {
    clear_runs()?;
    if json {
        writeln!(io::stdout().lock(), r#"{{"cleared": true}}"#)?;
    } else {
        runner
            .info("cleared run history")
            .map_err(HistoryError::Io)?;
    }
    Ok(())
}

fn format_target_kind(kind: nxr_core::RunTargetKind) -> &'static str {
    match kind {
        nxr_core::RunTargetKind::App => "app",
        nxr_core::RunTargetKind::Task => "task",
        nxr_core::RunTargetKind::WorkspaceScript => "workspace-script",
    }
}

/// Persist one completed invocation (best-effort).
pub fn record_completed_run(
    started: Instant,
    target_kind: RunTargetKind,
    target: String,
    flake: Option<String>,
    exit_code: i32,
    discovery_context: Option<&DiscoveryContext>,
    require_tasks: bool,
) {
    let (discovery_cache, discovery_miss_reasons) =
        discovery_outcome(discovery_context, require_tasks);
    record_run(RunSummaryInput {
        target_kind,
        target,
        flake,
        exit_code,
        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        discovery_cache,
        discovery_miss_reasons,
    });
}

fn discovery_outcome(
    context: Option<&DiscoveryContext>,
    require_tasks: bool,
) -> (DiscoveryCacheOutcome, Vec<String>) {
    let Some(context) = context else {
        return (DiscoveryCacheOutcome::NotApplicable, Vec::new());
    };
    let Ok(entry) = discovery_cache_entry_with_options(
        context,
        DiscoveryCacheOptions {
            refresh: false,
            require_tasks,
        },
    ) else {
        return (DiscoveryCacheOutcome::NotApplicable, Vec::new());
    };
    if !entry.available {
        return (DiscoveryCacheOutcome::NotApplicable, Vec::new());
    }
    if entry.hit {
        return (DiscoveryCacheOutcome::Hit, Vec::new());
    }
    (
        DiscoveryCacheOutcome::Miss,
        entry.miss_reasons.iter().map(miss_reason_label).collect(),
    )
}

fn miss_reason_label(reason: &DiscoveryCacheMissReason) -> String {
    serde_json::to_string(reason).unwrap_or_else(|_| format!("{reason:?}"))
}

#[cfg(test)]
mod tests {
    use super::{discovery_outcome, record_completed_run};
    use nxr_completion::DiscoveryContext;
    use nxr_core::{DiscoveryCacheOutcome, RunTargetKind};
    use std::time::Instant;

    #[test]
    fn discovery_outcome_without_context_is_not_applicable() {
        let (outcome, reasons) = discovery_outcome(None, false);
        assert_eq!(outcome, DiscoveryCacheOutcome::NotApplicable);
        assert!(reasons.is_empty());
    }

    #[test]
    fn record_completed_run_does_not_panic_without_context() {
        record_completed_run(
            Instant::now(),
            RunTargetKind::App,
            "hello".to_owned(),
            None,
            0,
            None,
            false,
        );
    }

    #[test]
    fn discovery_outcome_remote_flake_is_not_applicable() {
        let context = DiscoveryContext::new("github:foo/bar", None, "aarch64-darwin");
        let (outcome, _) = discovery_outcome(Some(&context), false);
        assert_eq!(outcome, DiscoveryCacheOutcome::NotApplicable);
    }
}
