//! Journal-first foreground execution and workspace-bound read access.

use std::path::{Path, PathBuf};

use nxr_task::run_journal::{ForegroundPayload, RunJournal, RunJournalReader};
use nxr_task::run_view::{RUN_PLAN_V1, RunPlanMetadata};
use nxr_task::{Event, EventSink, ExecutionPlan};

pub fn directory(run_id: &str) -> Result<PathBuf, String> {
    nxr_core::durable_run::validate_run_id(run_id).map_err(|error| error.to_string())?;
    Ok(nxr_core::state_dir().join("runs").join(run_id).join("dag"))
}

pub fn open(
    run_id: &str,
    workspace: &Path,
    plan: &ExecutionPlan,
    logs_captured: bool,
    input_provenance: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, nxr_task::InputProvenance>,
    >,
) -> Result<RunJournal, String> {
    let path = directory(run_id)?;
    std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let canonical = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut journal = RunJournal::open(&path)?;
    journal.set_foreground_plan(&RunPlanMetadata {
        schema_version: RUN_PLAN_V1.to_owned(),
        run_id: run_id.to_owned(),
        operation_id: format!("task:{}", plan.root),
        workspace_root: canonical.to_string_lossy().into_owned(),
        workspace_id: nxr_task::workspace_id(None, None, &canonical.to_string_lossy()),
        roots: if plan.roots.is_empty() {
            vec![plan.root.clone()]
        } else {
            plan.roots.clone()
        },
        dependencies: plan
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.depends_on.clone()))
            .collect(),
        logs_captured,
        input_provenance,
        uncaptured_nodes: plan
            .nodes
            .iter()
            .filter(|node| !logs_captured || node.interactive)
            .map(|node| node.id.clone())
            .collect(),
    })?;
    Ok(journal)
}

pub fn reader(run_id: &str, workspace: Option<&Path>) -> Result<RunJournalReader, String> {
    let reader = RunJournalReader::open(directory(run_id)?)?;
    if reader.plan()?.run_id != run_id {
        return Err("run journal identity does not match its directory".to_owned());
    }
    if let Some(workspace) = workspace {
        let canonical = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if reader.plan()?.workspace_root != canonical.to_string_lossy() {
            return Err("run belongs to another workspace".to_owned());
        }
    }
    Ok(reader)
}

pub struct RecordedSink<S> {
    inner: S,
    journal: RunJournal,
    error: Option<String>,
    directory: PathBuf,
    artifact_store: PathBuf,
    first_child_failure: Option<i32>,
    checked_artifacts: std::collections::BTreeMap<String, u64>,
}

impl<S> RecordedSink<S> {
    pub fn new(inner: S, journal: RunJournal, run_id: &str) -> Result<Self, String> {
        let directory = directory(run_id)?;
        let artifact_store =
            PathBuf::from(RunJournalReader::open(&directory)?.plan()?.workspace_root)
                .join(".nxr/artifacts");
        Ok(Self {
            inner,
            journal,
            error: None,
            directory,
            artifact_store,
            first_child_failure: None,
            checked_artifacts: Default::default(),
        })
    }

    fn verify_node_artifacts(&mut self, node: &str) -> Result<(), String> {
        let reader = RunJournalReader::open(&self.directory)?;
        let mut after = self.checked_artifacts.get(node).copied().unwrap_or(0);
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let page = reader.artifacts(Some(node), after, 256)?;
            if page.is_empty() {
                break;
            }
            for record in page {
                after = record.sequence;
                if let ForegroundPayload::ArtifactDeclared { artifact, .. } = record.payload {
                    if !seen.insert((
                        artifact.digest.clone(),
                        serde_json::to_string(&artifact.location)
                            .map_err(|error| error.to_string())?,
                    )) {
                        continue;
                    }
                    if seen.len() > 1024 {
                        return Err(
                            "node exceeded the diagnostic artifact verification limit".to_owned()
                        );
                    }
                    match artifact.verify_local_cas(&self.artifact_store) {
                        Ok(verified) => {
                            self.journal.publish_verified_artifact(node, &verified)?;
                        }
                        Err(message) => {
                            self.journal.record_artifact_failure(node, &message)?;
                            return Err(format!(
                                "artifact verification failed for {node}: {message}"
                            ));
                        }
                    }
                }
            }
        }
        self.checked_artifacts.insert(node.to_owned(), after);
        Ok(())
    }

    pub fn first_child_failure(&self) -> Option<i32> {
        self.first_child_failure
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: EventSink> EventSink for RecordedSink<S> {
    fn emit(&mut self, event: Event) {
        if self.error.is_some() {
            return;
        }
        if let Event::NodeExited {
            code: Some(code), ..
        } = &event
            && *code != 0
            && self.first_child_failure.is_none()
        {
            self.first_child_failure = Some(*code);
        }
        if let Err(message) = self.journal.record_foreground_event(&event) {
            self.error = Some(format!("run evidence persistence failed: {message}"));
            return;
        }
        if let Event::NodeExited { node, .. } = &event
            && let Err(message) = self.verify_node_artifacts(node)
        {
            self.error = Some(message);
            return;
        }
        self.inner.emit(event);
    }

    fn failure(&self) -> Option<&str> {
        self.error.as_deref().or_else(|| self.inner.failure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_run_names_cannot_select_other_state_files() {
        for id in ["", "../another", "a/b"] {
            assert!(directory(id).is_err());
        }
    }
}
