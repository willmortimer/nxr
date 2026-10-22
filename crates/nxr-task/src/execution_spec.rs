//! `execution-spec-v1` envelopes for workflow leaves.
//!
//! The JSON shape matches NixPlane `schemas/execution-spec-v1.schema.json`.
//! NXR does not depend on the NixPlane crate. Secret values are omitted;
//! only `nixplane://binding/…` references are copied.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::workflows::{WorkflowDocument, WorkflowPlatform, WorkflowTask};

pub const EXECUTION_SPEC_V1: &str = "execution-spec-v1";
pub const RUN_EVENT_V2: &str = "run-event-v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdmissionPolicy {
    pub cancel: String,
    pub jobs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputDecl {
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CiIdentity {
    pub provider: String,
    pub external_job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
}

/// One workflow leaf in the shared execution envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSpecV1 {
    pub schema_version: String,
    pub source_set_id: String,
    pub task_id: String,
    pub operation_id: String,
    pub platform: String,
    pub capabilities: Vec<String>,
    pub admission: AdmissionPolicy,
    pub secret_refs: Vec<String>,
    pub outputs: Vec<OutputDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiIdentity>,
    pub idempotency_key: String,
    pub leaf: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunEventV2 {
    pub schema_version: String,
    pub run_id: String,
    pub operation_id: String,
    pub sequence: u64,
    pub timestamp: String,
    pub event_type: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecRun {
    pub operation_id: String,
    pub exit_code: i32,
}

/// Identity of the workflow file bytes. Not a path and not a floating HEAD.
#[must_use]
pub fn source_set_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("src_{hex}")
}

/// Emit one spec per task. Order follows the document's sorted task names.
#[must_use]
pub fn execution_specs_for_document(
    document: &WorkflowDocument,
    source_set_id: &str,
) -> Vec<ExecutionSpecV1> {
    document
        .tasks
        .iter()
        .map(|(task_id, task)| execution_spec_for_task(source_set_id, task_id, task))
        .collect()
}

/// Build the envelope for one leaf. The operation id ignores the idempotency key.
#[must_use]
pub fn execution_spec_for_task(
    source_set_id: &str,
    task_id: &str,
    task: &WorkflowTask,
) -> ExecutionSpecV1 {
    let leaf = serde_json::to_value(&task.operation).unwrap_or(Value::Null);
    let operation_id = stable_operation_id(source_set_id, task_id, &leaf);
    let secret_refs = task
        .environment
        .values()
        .filter(|value| value.starts_with("nixplane://binding/"))
        .cloned()
        .collect();
    ExecutionSpecV1 {
        schema_version: EXECUTION_SPEC_V1.to_owned(),
        source_set_id: source_set_id.to_owned(),
        task_id: task_id.to_owned(),
        operation_id: operation_id.clone(),
        platform: platform_label(task.platform),
        capabilities: vec![task.operation.kind().to_owned()],
        admission: AdmissionPolicy {
            cancel: "cooperative".to_owned(),
            jobs: task.resources.cpu.max(1),
            timeout_ms: None,
        },
        secret_refs,
        outputs: task
            .artifacts
            .iter()
            .map(|name| OutputDecl { name: name.clone() })
            .collect(),
        parent_run_id: None,
        ci: None,
        idempotency_key: operation_id,
        leaf,
    }
}

/// Run a native leaf and append `started` plus `exited` to the cursor.
///
/// # Errors
///
/// Returns an error when the leaf is not a native command, the cursor cannot
/// be written, or the process cannot be spawned.
pub fn execute_spec_locally(
    spec: &ExecutionSpecV1,
    cursor_path: &Path,
    run_id: &str,
) -> Result<SpecRun, String> {
    let argv = native_argv(&spec.leaf)?;
    let mut cursor = EventCursor::open(cursor_path)?;
    let sequence = cursor.last_sequence() + 1;
    cursor.append(RunEventV2 {
        schema_version: RUN_EVENT_V2.to_owned(),
        run_id: run_id.to_owned(),
        operation_id: spec.operation_id.clone(),
        sequence,
        timestamp: crate::format_rfc3339_utc(std::time::SystemTime::now()),
        event_type: "started".to_owned(),
        source: "local".to_owned(),
        exit_code: None,
        log_ref: None,
    })?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    let status = command.status().map_err(|err| format!("spawn: {err}"))?;
    let exit_code = status.code().unwrap_or(1);
    cursor.append(RunEventV2 {
        schema_version: RUN_EVENT_V2.to_owned(),
        run_id: run_id.to_owned(),
        operation_id: spec.operation_id.clone(),
        sequence: sequence + 1,
        timestamp: crate::format_rfc3339_utc(std::time::SystemTime::now()),
        event_type: "exited".to_owned(),
        source: "local".to_owned(),
        exit_code: Some(exit_code),
        log_ref: Some(format!("log:{run_id}:{}", spec.operation_id)),
    })?;
    Ok(SpecRun {
        operation_id: spec.operation_id.clone(),
        exit_code,
    })
}

#[derive(Debug)]
pub struct EventCursor {
    path: PathBuf,
    events: Vec<RunEventV2>,
}

impl EventCursor {
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or a line is not an event.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|err| format!("create cursor dir: {err}"))?;
        }
        let events = if path.exists() {
            let file = File::open(&path).map_err(|err| format!("read cursor: {err}"))?;
            let mut events = Vec::new();
            for line in BufReader::new(file).lines() {
                let line = line.map_err(|err| format!("read cursor line: {err}"))?;
                if line.trim().is_empty() {
                    continue;
                }
                events.push(
                    serde_json::from_str(&line).map_err(|err| format!("parse cursor: {err}"))?,
                );
            }
            events
        } else {
            Vec::new()
        };
        Ok(Self { path, events })
    }

    #[must_use]
    pub fn last_sequence(&self) -> u64 {
        self.events.last().map(|event| event.sequence).unwrap_or(0)
    }

    #[must_use]
    pub fn events(&self) -> &[RunEventV2] {
        &self.events
    }

    /// # Errors
    ///
    /// Returns an error when `sequence` is not the next number or the write fails.
    pub fn append(&mut self, event: RunEventV2) -> Result<(), String> {
        let expected = self.last_sequence() + 1;
        if event.sequence != expected {
            return Err(format!(
                "event sequence {} does not follow {expected}",
                event.sequence
            ));
        }
        let mut line = serde_json::to_string(&event).map_err(|err| err.to_string())?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| format!("open cursor: {err}"))?;
        file.write_all(line.as_bytes())
            .map_err(|err| format!("append cursor: {err}"))?;
        self.events.push(event);
        Ok(())
    }
}

fn platform_label(platform: WorkflowPlatform) -> String {
    match platform {
        WorkflowPlatform::Any => "any".to_owned(),
        WorkflowPlatform::Unix => "unix".to_owned(),
        WorkflowPlatform::Macos => "macos".to_owned(),
        WorkflowPlatform::Linux => "linux".to_owned(),
        WorkflowPlatform::Windows => "windows".to_owned(),
    }
}

fn stable_operation_id(source_set_id: &str, task_id: &str, leaf: &Value) -> String {
    let leaf_text = serde_json::to_string(leaf).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(source_set_id.as_bytes());
    hasher.update([0]);
    hasher.update(task_id.as_bytes());
    hasher.update([0]);
    hasher.update(leaf_text.as_bytes());
    let digest = hasher.finalize();
    let mut id = String::from("op_");
    for byte in digest.iter().take(16) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

fn native_argv(leaf: &Value) -> Result<Vec<String>, String> {
    let kind = leaf.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "native_command" {
        return Err(format!(
            "local consumer runs native_command leaves, got `{kind}`"
        ));
    }
    let argv = leaf
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| "native leaf is missing argv".to_owned())?;
    let argv: Vec<String> = argv
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if argv.is_empty() {
        return Err("native argv is empty".to_owned());
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use nxr_core::Operation;

    use crate::resources::NodeResources;

    fn fixture_document() -> WorkflowDocument {
        let mut environment = BTreeMap::new();
        environment.insert(
            "ATTIC".to_owned(),
            "nixplane://binding/attic-token".to_owned(),
        );
        environment.insert("PLAIN".to_owned(), "do-not-publish".to_owned());
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "finish".to_owned(),
            WorkflowTask {
                depends_on: Vec::new(),
                platform: WorkflowPlatform::Any,
                architecture: None,
                resources: NodeResources::EMPTY,
                environment,
                working_directory: None,
                artifacts: vec!["out.txt".to_owned()],
                operation: Operation::NativeCommand {
                    argv: vec!["/usr/bin/true".to_owned()],
                },
            },
        );
        WorkflowDocument {
            schema_version: 1,
            tasks,
        }
    }

    #[test]
    fn the_same_operation_id_survives_a_second_consumer() {
        let document = fixture_document();
        let specs = execution_specs_for_document(&document, "src_fixture");
        assert_eq!(specs.len(), 1);
        let json = serde_json::to_string(&specs[0]).expect("serialize");
        assert!(!json.contains("do-not-publish"));
        assert!(json.contains("nixplane://binding/attic-token"));
        let parsed: ExecutionSpecV1 = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.operation_id, specs[0].operation_id);
        assert_eq!(parsed.operation_id.len(), 3 + 32);

        let dir = tempfile::tempdir().expect("temp");
        let cursor = dir.path().join("events.jsonl");
        let first = execute_spec_locally(&parsed, &cursor, "run_local").expect("first");
        let again = execute_spec_locally(&parsed, &dir.path().join("other.jsonl"), "run_remote")
            .expect("second");
        assert_eq!(first.operation_id, again.operation_id);
        assert_eq!(first.exit_code, 0);
        assert_eq!(again.exit_code, 0);

        drop(parsed);
        let resumed = EventCursor::open(&cursor).expect("resume");
        assert_eq!(resumed.last_sequence(), 2);
        assert_eq!(resumed.events()[0].event_type, "started");
        assert_eq!(resumed.events()[1].event_type, "exited");
        assert_eq!(resumed.events()[1].exit_code, Some(0));
        let mut resumed = resumed;
        let duplicate = resumed.append(resumed.events()[1].clone());
        assert!(duplicate.is_err());
    }
}
