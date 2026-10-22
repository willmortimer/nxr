//! `execution-spec-v2` envelopes for workflow leaves.
//!
//! The JSON shape matches NixPlane `schemas/execution-spec-v2.schema.json`.
//! NXR does not depend on the NixPlane crate. Binding destinations carry refs,
//! never plaintext. Ordinary values stay in the executable spec and are removed
//! by [`ExecutionSpecV2::redacted`] for public metadata.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::{
    BindingDestination, EXECUTION_SPEC_V2, ExecutionEnvironment, ExecutionResources,
    OperationIdentity, SourceSetV1, WorkingDirectoryPolicy, operation_id, source_set_digest,
};
use crate::run_journal::{JournalEvent, RunJournal};
use crate::workflows::{WorkflowDocument, WorkflowPlatform, WorkflowTask};

pub use crate::identity::{SOURCE_SET_V1, workflow_definition_digest};

pub const RUN_EVENT_V2: &str = "run-event-v2";
const BINDING_REF_PREFIX: &str = "nixplane://binding/";

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

/// One executable workflow leaf.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSpecV2 {
    pub schema_version: String,
    pub source_set_id: String,
    pub workflow_definition_digest: String,
    pub task_id: String,
    pub operation_id: String,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    pub capabilities: Vec<String>,
    pub admission: AdmissionPolicy,
    pub environment: ExecutionEnvironment,
    pub working_directory: WorkingDirectoryPolicy,
    pub resources: ExecutionResources,
    pub secret_refs: Vec<String>,
    pub outputs: Vec<OutputDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiIdentity>,
    pub idempotency_key: String,
    pub leaf: Value,
}

impl ExecutionSpecV2 {
    /// Identity preimage. CI and the idempotency key are not included.
    #[must_use]
    pub fn identity(&self) -> OperationIdentity {
        OperationIdentity {
            source_set_digest: self.source_set_id.clone(),
            workflow_definition_digest: self.workflow_definition_digest.clone(),
            task_id: self.task_id.clone(),
            leaf: self.leaf.clone(),
            platform: self.platform.clone(),
            architecture: self.architecture.clone().unwrap_or_default(),
            working_directory_kind: self.working_directory.kind.clone(),
            working_directory_path: self.working_directory.path.clone(),
            ordinary: self.environment.ordinary.clone(),
            bindings: self.environment.bindings.clone(),
            cancel: self.admission.cancel.clone(),
            jobs: self.admission.jobs,
            timeout_ms: self
                .admission
                .timeout_ms
                .map(|timeout| timeout.to_string())
                .unwrap_or_default(),
            cpu: self.resources.cpu,
            memory_bytes: self.resources.memory_bytes,
            exclusive: self.resources.exclusive.clone(),
            outputs: self
                .outputs
                .iter()
                .map(|output| output.name.clone())
                .collect(),
        }
    }

    #[must_use]
    pub fn recomputed_operation_id(&self) -> String {
        operation_id(&self.identity())
    }

    /// Ordinary values are cleared. Binding refs remain.
    #[must_use]
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        for value in copy.environment.ordinary.values_mut() {
            value.clear();
        }
        copy
    }
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

/// Emit one spec per task. Order follows the document's sorted task names.
///
/// # Errors
///
/// Returns an error when a task has an absolute working directory or a bad binding.
pub fn execution_specs_for_document(
    document: &WorkflowDocument,
    source: &SourceSetV1,
    workflow_digest: &str,
) -> Result<Vec<ExecutionSpecV2>, String> {
    document
        .tasks
        .iter()
        .map(|(task_id, task)| execution_spec_for_task(source, workflow_digest, task_id, task))
        .collect()
}

/// Build the envelope for one leaf.
///
/// # Errors
///
/// Returns an error when the working directory is not workspace-relative or a
/// binding ref is not `nixplane://binding/...`.
pub fn execution_spec_for_task(
    source: &SourceSetV1,
    workflow_digest: &str,
    task_id: &str,
    task: &WorkflowTask,
) -> Result<ExecutionSpecV2, String> {
    let leaf = serde_json::to_value(&task.operation).unwrap_or(Value::Null);
    let environment = split_environment(&task.environment)?;
    for binding in &environment.bindings {
        validate_binding(binding)?;
    }
    let working_directory = working_directory(task.working_directory.as_deref())?;
    let source_set_id = source_set_digest(source);
    let mut spec = ExecutionSpecV2 {
        schema_version: EXECUTION_SPEC_V2.to_owned(),
        source_set_id,
        workflow_definition_digest: workflow_digest.to_owned(),
        task_id: task_id.to_owned(),
        operation_id: String::new(),
        platform: platform_label(task.platform),
        architecture: task.architecture.clone(),
        capabilities: vec![task.operation.kind().to_owned()],
        admission: AdmissionPolicy {
            cancel: "cooperative".to_owned(),
            jobs: task.resources.cpu.max(1),
            timeout_ms: None,
        },
        secret_refs: environment
            .bindings
            .iter()
            .map(|binding| binding.reference.clone())
            .collect(),
        outputs: task
            .artifacts
            .iter()
            .map(|name| OutputDecl { name: name.clone() })
            .collect(),
        environment,
        working_directory,
        resources: ExecutionResources {
            cpu: task.resources.cpu,
            memory_bytes: task.resources.memory_bytes,
            exclusive: task.resources.exclusive.clone(),
        },
        parent_run_id: None,
        ci: None,
        idempotency_key: String::new(),
        leaf,
    };
    spec.operation_id = spec.recomputed_operation_id();
    spec.idempotency_key = spec.operation_id.clone();
    Ok(spec)
}

/// Run a native leaf when the spec matches this host.
///
/// Ordinary environment values are applied. Binding refs are not resolved.
///
/// # Errors
///
/// Returns an error when admission fails, the journal cannot be written, or
/// the process cannot be spawned.
pub fn execute_spec_locally(
    spec: &ExecutionSpecV2,
    journal_dir: &Path,
    run_id: &str,
) -> Result<SpecRun, String> {
    admit(spec)?;
    let argv = native_argv(&spec.leaf)?;
    let mut journal = RunJournal::open(journal_dir)?;
    let sequence = journal.last_sequence()? + 1;
    journal.append(journal_event(spec, run_id, sequence, "started", None, None))?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    for (key, value) in &spec.environment.ordinary {
        command.env(key, value);
    }
    let output = command.output().map_err(|err| format!("spawn: {err}"))?;
    let exit_code = output.status.code().unwrap_or(1);
    let event_type = if output.status.code().is_none() {
        "cancelled"
    } else {
        "exited"
    };
    let log_ref = format!("log_{run_id}_{}", spec.operation_id);
    let mut log_bytes = output.stdout;
    log_bytes.extend_from_slice(&output.stderr);
    journal.write_log(&log_ref, &log_bytes)?;
    journal.append(journal_event(
        spec,
        run_id,
        sequence + 1,
        event_type,
        Some(exit_code),
        Some(log_ref),
    ))?;
    Ok(SpecRun {
        operation_id: spec.operation_id.clone(),
        exit_code,
    })
}

fn journal_event(
    spec: &ExecutionSpecV2,
    run_id: &str,
    sequence: u64,
    event_type: &str,
    exit_code: Option<i32>,
    log_ref: Option<String>,
) -> JournalEvent {
    JournalEvent {
        schema_version: RUN_EVENT_V2.to_owned(),
        run_id: run_id.to_owned(),
        operation_id: spec.operation_id.clone(),
        sequence,
        timestamp: crate::format_rfc3339_utc(std::time::SystemTime::now()),
        event_type: event_type.to_owned(),
        source: "local".to_owned(),
        exit_code,
        log_ref,
    }
}

fn admit(spec: &ExecutionSpecV2) -> Result<(), String> {
    if spec.recomputed_operation_id() != spec.operation_id {
        return Err("operation id does not match the execution inputs".to_owned());
    }
    if !spec
        .capabilities
        .iter()
        .any(|capability| capability == "native_command")
    {
        return Err("missing capability native_command".to_owned());
    }
    if !platform_matches_host(&spec.platform) {
        return Err(format!(
            "platform `{}` does not match host `{}`",
            spec.platform,
            std::env::consts::OS
        ));
    }
    if let Some(architecture) = &spec.architecture
        && architecture != std::env::consts::ARCH
    {
        return Err(format!(
            "architecture `{architecture}` does not match host `{}`",
            std::env::consts::ARCH
        ));
    }
    for binding in &spec.environment.bindings {
        validate_binding(binding)?;
    }
    if spec.working_directory.kind != "workspace_relative" {
        return Err("working directory must be workspace_relative".to_owned());
    }
    Ok(())
}

fn validate_binding(binding: &BindingDestination) -> Result<(), String> {
    if !binding.reference.starts_with(BINDING_REF_PREFIX) {
        return Err(format!(
            "binding `{}` ref is not a nixplane binding",
            binding.name
        ));
    }
    if binding.delivery != "environment" && binding.delivery != "file" {
        return Err(format!(
            "binding `{}` delivery `{}` is unknown",
            binding.name, binding.delivery
        ));
    }
    Ok(())
}

fn split_environment(
    environment: &BTreeMap<String, String>,
) -> Result<ExecutionEnvironment, String> {
    let mut ordinary = BTreeMap::new();
    let mut bindings = Vec::new();
    for (name, value) in environment {
        if value.starts_with(BINDING_REF_PREFIX) {
            bindings.push(BindingDestination {
                name: name.clone(),
                reference: value.clone(),
                delivery: "environment".to_owned(),
            });
        } else if value.starts_with("nixplane://") {
            return Err(format!("environment `{name}` is not a binding ref"));
        } else {
            ordinary.insert(name.clone(), value.clone());
        }
    }
    Ok(ExecutionEnvironment { ordinary, bindings })
}

fn working_directory(path: Option<&str>) -> Result<WorkingDirectoryPolicy, String> {
    let path = path.unwrap_or(".");
    if path.starts_with('/') || path.starts_with('\\') || path.contains("..") {
        return Err("working directory must be workspace-relative".to_owned());
    }
    Ok(WorkingDirectoryPolicy {
        kind: "workspace_relative".to_owned(),
        path: path.to_owned(),
    })
}

fn platform_matches_host(platform: &str) -> bool {
    let os = std::env::consts::OS;
    match platform {
        "any" => true,
        "unix" => os == "macos" || os == "linux",
        "macos" => os == "macos",
        "linux" => os == "linux",
        "windows" => os == "windows",
        _ => false,
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

/// Read a git checkout into a source set. Dirty identity hashes status and diff.
///
/// # Errors
///
/// Returns an error when `git rev-parse HEAD` fails.
pub fn source_set_from_repo(root: &Path) -> Result<SourceSetV1, String> {
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let porcelain = git(root, &["status", "--porcelain"]).unwrap_or_default();
    let diff = git(root, &["diff", "HEAD"]).unwrap_or_default();
    let dirty_snapshot_id = if porcelain.trim().is_empty() {
        None
    } else {
        let mut material = String::new();
        material.push_str(&porcelain);
        material.push('\n');
        material.push_str(&diff);
        Some(format!(
            "dirty_{}",
            crate::identity::workflow_definition_digest(material.as_bytes())
                .trim_start_matches("wf_")
        ))
    };
    let mut submodules = Vec::new();
    if let Ok(status) = git(root, &["submodule", "status"]) {
        for line in status.lines() {
            let line = line.trim().trim_start_matches(['+', '-', 'U']);
            let mut parts = line.split_whitespace();
            let Some(commit) = parts.next() else { continue };
            let Some(path) = parts.next() else { continue };
            if commit.len() >= 7 {
                submodules.push(crate::identity::SubmodulePin {
                    path: path.to_owned(),
                    commit: commit.to_owned(),
                });
            }
        }
    }
    let mut lockfiles = Vec::new();
    for name in ["flake.lock", "Cargo.lock"] {
        let path = root.join(name);
        if path.is_file() {
            let bytes = std::fs::read(&path).map_err(|err| format!("read {name}: {err}"))?;
            lockfiles.push(crate::identity::LockfileDigest {
                path: name.to_owned(),
                digest: format!("sha256:{}", hex_sha256(&bytes)),
            });
        }
    }
    Ok(SourceSetV1 {
        schema_version: SOURCE_SET_V1.to_owned(),
        kind: "git".to_owned(),
        commit: commit.trim().to_owned(),
        submodules,
        lockfiles,
        dirty_snapshot_id,
    })
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed",
            args.first().copied().unwrap_or("command")
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use nxr_core::Operation;

    use crate::identity::workflow_definition_digest;
    use crate::resources::NodeResources;

    fn fixture_task() -> WorkflowTask {
        let mut environment = BTreeMap::new();
        environment.insert(
            "ATTIC".to_owned(),
            "nixplane://binding/attic-token".to_owned(),
        );
        environment.insert("PLAIN".to_owned(), "do-not-publish".to_owned());
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
        }
    }

    fn fixture_spec(commit: &str) -> ExecutionSpecV2 {
        let source = SourceSetV1::fixture(commit);
        execution_spec_for_task(
            &source,
            &workflow_definition_digest(b"schema_version = 1\n"),
            "finish",
            &fixture_task(),
        )
        .expect("spec")
    }

    #[test]
    fn source_revision_changes_operation_id_and_ci_does_not() {
        let first = fixture_spec("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let second = fixture_spec("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(first.source_set_id, second.source_set_id);
        assert_ne!(first.operation_id, second.operation_id);
        assert_eq!(first.operation_id.len(), 3 + 32);
        let mut with_ci = first.clone();
        with_ci.ci = Some(CiIdentity {
            provider: "github".to_owned(),
            external_job_id: "1/build/1".to_owned(),
            run_url: None,
        });
        with_ci.idempotency_key = "github:1/build/1:finish".to_owned();
        assert_eq!(with_ci.recomputed_operation_id(), first.operation_id);
        let redacted = serde_json::to_string(&first.redacted()).expect("redacted");
        assert!(!redacted.contains("do-not-publish"));
        assert!(redacted.contains("nixplane://binding/attic-token"));
        let full = serde_json::to_string(&first).expect("full");
        assert!(full.contains("do-not-publish"));
    }

    #[test]
    fn golden_fixture_matches_the_recomputed_operation_id() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/execution-spec-v2-native.json");
        let text = std::fs::read_to_string(&path).expect("golden");
        let spec: ExecutionSpecV2 = serde_json::from_str(&text).expect("parse");
        assert_eq!(spec.operation_id, "op_bac7776d6de1a951f25adc16b7bbade9");
        assert_eq!(spec.recomputed_operation_id(), spec.operation_id);
        let rebuilt = fixture_spec("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(rebuilt.operation_id, spec.operation_id);
        assert_eq!(rebuilt.source_set_id, spec.source_set_id);
    }

    #[test]
    fn the_same_operation_id_survives_a_second_consumer() {
        let spec = fixture_spec("fixture-commit");
        let json = serde_json::to_string(&spec).expect("serialize");
        let parsed: ExecutionSpecV2 = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.operation_id, spec.operation_id);
        assert_eq!(parsed.recomputed_operation_id(), spec.operation_id);

        let dir = tempfile::tempdir().expect("temp");
        let first =
            execute_spec_locally(&parsed, &dir.path().join("local"), "run_local").expect("first");
        let again = execute_spec_locally(&parsed, &dir.path().join("remote"), "run_remote")
            .expect("second");
        assert_eq!(first.operation_id, again.operation_id);
        assert_eq!(first.exit_code, 0);
        assert_eq!(again.exit_code, 0);

        let resumed = RunJournal::open(dir.path().join("local")).expect("resume");
        assert_eq!(resumed.last_sequence().expect("sequence"), 2);
        let events = resumed.replay_after(0, 10).expect("events");
        assert_eq!(events[0].event_type, "started");
        assert_eq!(events[1].event_type, "exited");
        assert_eq!(events[1].exit_code, Some(0));
        let log_ref = events[1].log_ref.clone().expect("log");
        assert!(resumed.read_log(&log_ref).is_ok());
        let mut duplicate = events[1].clone();
        let mut writer = RunJournal::open(dir.path().join("local-dup")).expect("dup");
        writer.append(events[0].clone()).expect("first row");
        duplicate.sequence = 1;
        assert!(writer.append(duplicate).is_err());
    }

    #[test]
    fn mismatched_architecture_is_rejected() {
        let mut spec = fixture_spec("fixture-commit");
        spec.architecture = Some("not-a-real-arch".to_owned());
        spec.operation_id = spec.recomputed_operation_id();
        let dir = tempfile::tempdir().expect("temp");
        let error = execute_spec_locally(&spec, dir.path(), "run").expect_err("reject");
        assert!(error.contains("architecture"));
    }
}
