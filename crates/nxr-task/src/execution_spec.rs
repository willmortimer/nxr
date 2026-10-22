//! `execution-spec-v2` envelopes for workflow leaves.
//!
//! The JSON shape matches NixPlane `schemas/execution-spec-v2.schema.json`.
//! NXR does not depend on the NixPlane crate. Binding destinations carry refs,
//! never plaintext. Ordinary values stay in the executable spec and are removed
//! by [`ExecutionSpecV2::redacted`] for public metadata.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

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
///
/// Unknown fields are rejected. Dirty bytes are not part of this document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    let mut sequence = journal.last_sequence()? + 1;
    journal.append(journal_event(spec, run_id, sequence, "started", None, None))?;
    let cwd = workspace_cwd(spec, journal_dir)?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir(&cwd);
    for (key, value) in &spec.environment.ordinary {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|err| format!("spawn: {err}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout pipe is missing".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "stderr pipe is missing".to_owned())?;
    let (tx, rx) = mpsc::sync_channel::<OutputChunk>(8);
    spawn_output_reader(tx.clone(), stdout, "stdout");
    spawn_output_reader(tx, stderr, "stderr");
    while let Ok(chunk) = rx.recv() {
        sequence += 1;
        sequence = record_chunk(&mut journal, spec, run_id, sequence, &chunk)?;
    }
    let status = child.wait().map_err(|err| format!("wait: {err}"))?;
    let exit_code = status.code().unwrap_or(1);
    let event_type = if status.code().is_none() {
        "cancelled"
    } else {
        "exited"
    };
    if exit_code == 0 {
        require_outputs(spec, &cwd)?;
    }
    sequence += 1;
    journal.append(journal_event(
        spec,
        run_id,
        sequence,
        event_type,
        Some(exit_code),
        None,
    ))?;
    Ok(SpecRun {
        operation_id: spec.operation_id.clone(),
        exit_code,
    })
}

struct OutputChunk {
    source: &'static str,
    bytes: Vec<u8>,
}

fn spawn_output_reader<R>(tx: mpsc::SyncSender<OutputChunk>, stream: R, source: &'static str)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if tx
                        .send(OutputChunk {
                            source,
                            bytes: buffer[..read].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

fn record_chunk(
    journal: &mut RunJournal,
    spec: &ExecutionSpecV2,
    run_id: &str,
    sequence: u64,
    chunk: &OutputChunk,
) -> Result<u64, String> {
    let log_ref = format!("sha256:{}", hex_sha256(&chunk.bytes));
    journal.write_log(&log_ref, &chunk.bytes)?;
    let mut event = journal_event(spec, run_id, sequence, "log", None, Some(log_ref));
    event.source = chunk.source.to_owned();
    journal.append(event)?;
    Ok(sequence)
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

fn workspace_cwd(spec: &ExecutionSpecV2, journal_dir: &Path) -> Result<PathBuf, String> {
    if spec.working_directory.kind != "workspace_relative" {
        return Err("working directory must be workspace_relative".to_owned());
    }
    let path = spec.working_directory.path.trim();
    let relative = if path.is_empty() || path == "." {
        PathBuf::new()
    } else {
        safe_output_name(path)?
    };
    let cwd = journal_dir.join(relative);
    fs::create_dir_all(&cwd).map_err(|err| format!("working directory: {err}"))?;
    Ok(cwd)
}

fn require_outputs(spec: &ExecutionSpecV2, cwd: &Path) -> Result<(), String> {
    for output in &spec.outputs {
        let relative = safe_output_name(&output.name)?;
        let path = cwd.join(relative);
        if !path.is_file() {
            return Err(format!("output {} is missing", output.name));
        }
    }
    Ok(())
}

fn safe_output_name(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(format!("output {path} escapes the workspace"));
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("output {path} escapes the workspace"));
    }
    Ok(relative.to_path_buf())
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

/// Read a git checkout into a source set.
///
/// Dirty identity is a sorted inventory of path, mode, and content hash or
/// symlink target. Ignored files stay out.
///
/// # Errors
///
/// Returns an error when `git rev-parse HEAD` fails.
pub fn source_set_from_repo(root: &Path) -> Result<SourceSetV1, String> {
    source_set_from_repo_inputs(root, &[])
}

/// Like [`source_set_from_repo`], and also hash `explicit_inputs` even when ignored.
///
/// # Errors
///
/// Returns an error when `git rev-parse HEAD` fails or an explicit input cannot be read.
pub fn source_set_from_repo_inputs(
    root: &Path,
    explicit_inputs: &[&str],
) -> Result<SourceSetV1, String> {
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let dirty_snapshot_id = dirty_snapshot_id(root, explicit_inputs)?;
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

/// Check out `expected` from `remote` and refuse the tree when the digest differs.
///
/// # Errors
///
/// Returns an error when clone, checkout, a submodule pin, or the digest check fails.
pub fn checkout_source(
    destination: &Path,
    remote: &Path,
    expected: &SourceSetV1,
) -> Result<(), String> {
    if expected.kind != "git" {
        return Err("only a git source set can be checked out".to_owned());
    }
    if destination.exists() {
        return Err("checkout destination already exists".to_owned());
    }
    let status = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(remote)
        .arg(destination)
        .status()
        .map_err(|err| format!("git clone: {err}"))?;
    if !status.success() {
        return Err("git clone failed".to_owned());
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["checkout", "--detach", "--quiet", expected.commit.trim()])
        .status()
        .map_err(|err| format!("git checkout: {err}"))?;
    if !status.success() {
        return Err("git checkout of the source commit failed".to_owned());
    }
    if !expected.submodules.is_empty() {
        let status = Command::new("git")
            .arg("-C")
            .arg(destination)
            .args(["submodule", "update", "--init", "--recursive"])
            .status()
            .map_err(|err| format!("git submodule: {err}"))?;
        if !status.success() {
            return Err("submodule pins could not be checked out".to_owned());
        }
    }
    verify_source_checkout(destination, expected)
}

/// Recompute the source-set digest after checkout and refuse a mismatch.
///
/// # Errors
///
/// Returns an error when the commit, a submodule pin, or the digest does not match.
pub fn verify_source_checkout(root: &Path, expected: &SourceSetV1) -> Result<(), String> {
    let actual = source_set_from_repo(root)?;
    if actual.commit != expected.commit.trim() {
        return Err(format!(
            "checked out commit {} does not match {}",
            actual.commit,
            expected.commit.trim()
        ));
    }
    for pin in &expected.submodules {
        let found = actual
            .submodules
            .iter()
            .any(|item| item.path == pin.path && item.commit == pin.commit);
        if !found {
            return Err(format!(
                "submodule `{}` is not pinned to {}",
                pin.path, pin.commit
            ));
        }
    }
    let actual_digest = source_set_digest(&actual);
    let expected_digest = source_set_digest(expected);
    if actual_digest != expected_digest {
        return Err(format!(
            "source set digest {actual_digest} does not match {expected_digest}"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceEntry {
    path: String,
    mode: String,
    kind: String,
    payload: String,
}

fn dirty_snapshot_id(root: &Path, explicit_inputs: &[&str]) -> Result<Option<String>, String> {
    let mut entries = dirty_entries(root)?;
    for path in explicit_inputs {
        if entries.iter().any(|entry| entry.path == *path) {
            continue;
        }
        if let Some(entry) = entry_for_worktree(root, path)? {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(digest_source_entries(&entries)))
}

fn dirty_entries(root: &Path) -> Result<Vec<SourceEntry>, String> {
    let porcelain = git(
        root,
        &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
    )
    .unwrap_or_default();
    parse_porcelain(root, &porcelain)
}

fn parse_porcelain(root: &Path, porcelain: &str) -> Result<Vec<SourceEntry>, String> {
    let mut entries = Vec::new();
    let mut rest = porcelain;
    while !rest.is_empty() {
        let (record, next) = split_nul(rest);
        rest = next;
        if record.len() < 3 {
            continue;
        }
        let status = &record[..2];
        let path = record[3..].to_owned();
        if status.starts_with('R') || status.starts_with('C') {
            let (dest, next) = split_nul(rest);
            rest = next;
            entries.push(tombstone(&path));
            if let Some(entry) = entry_for_worktree(root, dest)? {
                entries.push(entry);
            }
            continue;
        }
        let deleted =
            status.as_bytes().get(1) == Some(&b'D') || status.as_bytes().first() == Some(&b'D');
        if deleted && !root.join(&path).exists() {
            entries.push(tombstone(&path));
            continue;
        }
        if let Some(entry) = entry_for_worktree(root, &path)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn split_nul(text: &str) -> (&str, &str) {
    match text.find('\0') {
        Some(index) => (&text[..index], &text[index + 1..]),
        None => (text, ""),
    }
}

fn tombstone(path: &str) -> SourceEntry {
    SourceEntry {
        path: path.replace('\\', "/"),
        mode: "000000".to_owned(),
        kind: "deleted".to_owned(),
        payload: String::new(),
    }
}

fn entry_for_worktree(root: &Path, path: &str) -> Result<Option<SourceEntry>, String> {
    let full = root.join(path);
    let meta = match fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(_) => return Ok(Some(tombstone(path))),
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(&full).map_err(|err| format!("read link {path}: {err}"))?;
        return Ok(Some(SourceEntry {
            path: path.replace('\\', "/"),
            mode: "120000".to_owned(),
            kind: "symlink".to_owned(),
            payload: target.to_string_lossy().into_owned(),
        }));
    }
    if meta.is_dir() {
        let commit = git(&full, &["rev-parse", "HEAD"]).unwrap_or_default();
        let nested = dirty_entries(&full).unwrap_or_default();
        let mut payload = commit.trim().to_owned();
        if !nested.is_empty() {
            if !payload.is_empty() {
                payload.push('\n');
            }
            payload.push_str(&digest_source_entries(&nested));
        }
        return Ok(Some(SourceEntry {
            path: path.replace('\\', "/"),
            mode: "040000".to_owned(),
            kind: "directory".to_owned(),
            payload,
        }));
    }
    let bytes = fs::read(&full).map_err(|err| format!("read {path}: {err}"))?;
    Ok(Some(SourceEntry {
        path: path.replace('\\', "/"),
        mode: file_mode(&meta),
        kind: "file".to_owned(),
        payload: hex_sha256(&bytes),
    }))
}

fn file_mode(meta: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return "100755".to_owned();
        }
    }
    let _ = meta;
    "100644".to_owned()
}

fn digest_source_entries(entries: &[SourceEntry]) -> String {
    use sha2::Digest;
    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = sha2::Sha256::new();
    for entry in ordered {
        for field in [&entry.path, &entry.mode, &entry.kind, &entry.payload] {
            let bytes = field.as_bytes();
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
    }
    format!("dirty_{}", hex_sha256_bytes(hasher.finalize().as_slice()))
}

#[cfg(test)]
fn tree_digest(root: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    scan_tree(root, Path::new(""), &mut entries)?;
    if entries.is_empty() {
        return Ok(digest_source_entries(&[]));
    }
    Ok(digest_source_entries(&entries))
}

#[cfg(test)]
fn scan_tree(root: &Path, relative: &Path, entries: &mut Vec<SourceEntry>) -> Result<(), String> {
    let directory = root.join(relative);
    let mut names = fs::read_dir(&directory)
        .map_err(|err| format!("read {}: {err}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("read dir: {err}"))?;
    names.sort_by_key(|entry| entry.file_name());
    for entry in names {
        let name = entry.file_name();
        let path = relative.join(&name);
        let display = path.to_string_lossy().replace('\\', "/");
        let full = root.join(&path);
        let meta = fs::symlink_metadata(&full).map_err(|err| format!("stat {display}: {err}"))?;
        if meta.file_type().is_symlink() {
            let target =
                fs::read_link(&full).map_err(|err| format!("read link {display}: {err}"))?;
            entries.push(SourceEntry {
                path: display,
                mode: "120000".to_owned(),
                kind: "symlink".to_owned(),
                payload: target.to_string_lossy().into_owned(),
            });
            continue;
        }
        if meta.is_dir() {
            scan_tree(root, &path, entries)?;
            continue;
        }
        let bytes = fs::read(&full).map_err(|err| format!("read {display}: {err}"))?;
        entries.push(SourceEntry {
            path: display,
            mode: file_mode(&meta),
            kind: "file".to_owned(),
            payload: hex_sha256(&bytes),
        });
    }
    Ok(())
}

#[cfg(test)]
fn materialize_files(root: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    for (path, bytes) in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("mkdir: {err}"))?;
        }
        fs::write(&full, bytes).map_err(|err| format!("write {path}: {err}"))?;
    }
    Ok(())
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
    hex_sha256_bytes(Sha256::digest(bytes).as_slice())
}

fn hex_sha256_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One dirty worktree path, including the bytes a worker must write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirtyFile {
    /// Repo-relative path.
    pub path: String,
    /// Git mode (`100644`, `100755`, `120000`, or `000000`).
    pub mode: String,
    /// `file`, `symlink`, `directory`, or `deleted`.
    pub kind: String,
    /// Content hash, symlink target, or empty tombstone.
    pub payload: String,
    /// File bytes. Empty for symlinks, directories, and deletions.
    pub bytes: Vec<u8>,
}

/// Read the dirty inventory as whole files. Ignored paths stay out.
///
/// # Errors
///
/// Returns an error when git status or a dirty file cannot be read.
pub fn capture_dirty_files(root: &Path) -> Result<Vec<DirtyFile>, String> {
    let entries = dirty_entries(root)?;
    let mut files = Vec::with_capacity(entries.len());
    for entry in entries {
        let bytes = if entry.kind == "file" {
            fs::read(root.join(&entry.path)).map_err(|err| format!("read {}: {err}", entry.path))?
        } else {
            Vec::new()
        };
        files.push(DirtyFile {
            path: entry.path,
            mode: entry.mode,
            kind: entry.kind,
            payload: entry.payload,
            bytes,
        });
    }
    Ok(files)
}

/// Digest of `files`. File payloads are the sha256 of `bytes`.
#[must_use]
pub fn dirty_files_digest(files: &[DirtyFile]) -> String {
    let entries = files
        .iter()
        .map(|file| SourceEntry {
            path: file.path.clone(),
            mode: file.mode.clone(),
            kind: file.kind.clone(),
            payload: if file.kind == "file" {
                hex_sha256(&file.bytes)
            } else {
                file.payload.clone()
            },
        })
        .collect::<Vec<_>>();
    digest_source_entries(&entries)
}

/// Write `files` into `destination`. Deletions remove a path that is present.
///
/// Paths are staged, then moved into `destination` only after every path is inside it.
///
/// # Errors
///
/// Returns an error when a path escapes `destination` or cannot be created.
pub fn materialize_dirty_files(destination: &Path, files: &[DirtyFile]) -> Result<(), String> {
    let staging = destination.join(".nixplane-stage");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|err| format!("staging: {err}"))?;
    for file in files {
        let relative = safe_relative(&file.path)?;
        reject_symlink_parent(destination, &relative)?;
        let path = staging.join(&relative);
        match file.kind.as_str() {
            "deleted" => {}
            "directory" => {
                fs::create_dir_all(&path).map_err(|err| format!("mkdir {}: {err}", file.path))?;
            }
            "symlink" => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|err| format!("mkdir: {err}"))?;
                }
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&file.payload, &path)
                        .map_err(|err| format!("symlink {}: {err}", file.path))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = path;
                    return Err(format!(
                        "symlink {} is not supported on this host",
                        file.path
                    ));
                }
            }
            _ => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|err| format!("mkdir: {err}"))?;
                }
                fs::write(&path, &file.bytes)
                    .map_err(|err| format!("write {}: {err}", file.path))?;
                #[cfg(unix)]
                if file.mode == "100755" {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = fs::metadata(&path)
                        .map_err(|err| format!("stat {}: {err}", file.path))?
                        .permissions();
                    permissions.set_mode(0o755);
                    fs::set_permissions(&path, permissions)
                        .map_err(|err| format!("chmod {}: {err}", file.path))?;
                }
            }
        }
    }
    for file in files {
        if file.kind == "deleted" {
            let relative = safe_relative(&file.path)?;
            let path = destination.join(&relative);
            if path.exists() {
                fs::remove_file(&path).map_err(|err| format!("remove {}: {err}", file.path))?;
            }
        }
    }
    activate_staging(destination, &staging)?;
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

fn safe_relative(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(format!("path {path} escapes the workspace"));
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(format!("path {path} escapes the workspace"));
    }
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!("path {path} escapes the workspace"));
        }
    }
    Ok(relative.to_path_buf())
}

fn reject_symlink_parent(root: &Path, relative: &Path) -> Result<(), String> {
    let mut cursor = root.to_path_buf();
    if let Ok(meta) = fs::symlink_metadata(&cursor)
        && meta.file_type().is_symlink()
    {
        return Err("destination escapes the workspace".to_owned());
    }
    for component in relative.components() {
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("path {} escapes the workspace", relative.display()));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

fn activate_staging(destination: &Path, staging: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(staging) {
        Ok(entries) => entries,
        Err(err) => return Err(format!("stage: {err}")),
    };
    for entry in entries {
        let entry = entry.map_err(|err| format!("stage: {err}"))?;
        if entry.file_name() == ".nixplane-stage" {
            continue;
        }
        move_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn move_tree(from: &Path, to: &Path) -> Result<(), String> {
    let target_is_dir = to
        .symlink_metadata()
        .map(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        .unwrap_or(false);
    if target_is_dir {
        for entry in fs::read_dir(from).map_err(|err| format!("stage: {err}"))? {
            let entry = entry.map_err(|err| format!("stage: {err}"))?;
            move_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        let _ = fs::remove_dir(from);
        return Ok(());
    }
    if to.exists() {
        fs::remove_file(to).map_err(|err| format!("replace {}: {err}", to.display()))?;
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("mkdir: {err}"))?;
    }
    fs::rename(from, to).map_err(|err| format!("activate {}: {err}", to.display()))
}

/// Recompute the digest and check the written paths.
///
/// # Errors
///
/// Returns an error when the digest or a materialized path does not match.
pub fn verify_materialized_dirty_files(
    destination: &Path,
    files: &[DirtyFile],
    expected_digest: &str,
) -> Result<(), String> {
    let digest = dirty_files_digest(files);
    if digest != expected_digest {
        return Err(format!(
            "source set digest {digest} does not match {expected_digest}"
        ));
    }
    for file in files {
        let path = destination.join(&file.path);
        match file.kind.as_str() {
            "deleted" => {
                if path.exists() {
                    return Err(format!("deleted path {} is still present", file.path));
                }
            }
            "symlink" => {
                let target =
                    fs::read_link(&path).map_err(|err| format!("link {}: {err}", file.path))?;
                if target.to_string_lossy() != file.payload {
                    return Err(format!("symlink {} target does not match", file.path));
                }
            }
            "directory" => {
                if !path.is_dir() {
                    return Err(format!("directory {} was not created", file.path));
                }
            }
            _ => {
                let bytes = fs::read(&path).map_err(|err| format!("read {}: {err}", file.path))?;
                if bytes != file.bytes || hex_sha256(&bytes) != file.payload {
                    return Err(format!(
                        "file {} does not match the source digest",
                        file.path
                    ));
                }
            }
        }
    }
    Ok(())
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
        for name in ["local", "remote"] {
            let journal = dir.path().join(name);
            fs::create_dir_all(&journal).expect("journal");
            fs::write(journal.join("out.txt"), b"present").expect("output");
        }
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
        assert!(events[1].log_ref.is_none());
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

    fn git_repo(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("-c")
            .arg("user.email=nxr@example.com")
            .arg("-c")
            .arg("user.name=nxr")
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn untracked_bytes_change_the_dirty_digest() {
        let dir = tempfile::tempdir().expect("temp");
        let root = dir.path();
        git_repo(root, &["init"]);
        fs::write(root.join("tracked"), "base").expect("write");
        git_repo(root, &["add", "tracked"]);
        git_repo(root, &["commit", "-m", "base"]);
        fs::write(root.join(".gitignore"), "ignored.txt\n").expect("ignore");
        git_repo(root, &["add", ".gitignore"]);
        git_repo(root, &["commit", "-m", "ignore"]);
        fs::write(root.join("ignored.txt"), "secret").expect("ignored");
        fs::write(root.join("extra.txt"), "one").expect("extra");
        let first = source_set_from_repo(root).expect("first").dirty_snapshot_id;
        fs::write(root.join("ignored.txt"), "changed").expect("ignored change");
        let ignored = source_set_from_repo(root)
            .expect("ignored")
            .dirty_snapshot_id;
        assert_eq!(first, ignored);
        fs::write(root.join("extra.txt"), "two").expect("extra change");
        let second = source_set_from_repo(root)
            .expect("second")
            .dirty_snapshot_id;
        assert_ne!(first, second);
        let named = source_set_from_repo_inputs(root, &["ignored.txt"])
            .expect("named")
            .dirty_snapshot_id;
        fs::write(root.join("ignored.txt"), "again").expect("named change");
        let named_again = source_set_from_repo_inputs(root, &["ignored.txt"])
            .expect("named again")
            .dirty_snapshot_id;
        assert_ne!(named, named_again);
    }

    #[test]
    fn a_dirty_file_materializes_with_the_same_digest() {
        let dir = tempfile::tempdir().expect("temp");
        let root = dir.path();
        git_repo(root, &["init"]);
        fs::write(root.join("tracked"), "base").expect("write");
        git_repo(root, &["add", "tracked"]);
        git_repo(root, &["commit", "-m", "base"]);
        fs::write(root.join("extra.txt"), "one").expect("extra");
        let expected = source_set_from_repo(root)
            .expect("source")
            .dirty_snapshot_id
            .expect("dirty");
        let files = capture_dirty_files(root).expect("capture");
        assert_eq!(dirty_files_digest(&files), expected);
        let dest = tempfile::tempdir().expect("dest");
        materialize_dirty_files(dest.path(), &files).expect("materialize");
        verify_materialized_dirty_files(dest.path(), &files, &expected).expect("verify");
        assert_eq!(
            fs::read(dest.path().join("extra.txt")).expect("read"),
            b"one"
        );
        let mut mutated = files.clone();
        mutated[0].bytes.push(b'x');
        assert_ne!(dirty_files_digest(&mutated), expected);
    }

    #[test]
    fn a_dirty_path_cannot_escape_the_destination() {
        let dest = tempfile::tempdir().expect("dest");
        let escape = DirtyFile {
            path: "../escape.txt".to_owned(),
            mode: "100644".to_owned(),
            kind: "file".to_owned(),
            payload: String::new(),
            bytes: b"no".to_vec(),
        };
        assert!(materialize_dirty_files(dest.path(), &[escape]).is_err());
        assert!(
            !dest
                .path()
                .parent()
                .expect("parent")
                .join("escape.txt")
                .exists()
        );
        let absolute = DirtyFile {
            path: "/etc/passwd".to_owned(),
            mode: "100644".to_owned(),
            kind: "file".to_owned(),
            payload: String::new(),
            bytes: b"no".to_vec(),
        };
        assert!(materialize_dirty_files(dest.path(), &[absolute]).is_err());
    }

    #[test]
    fn the_same_inventory_has_one_digest() {
        let left = tempfile::tempdir().expect("left");
        let right = tempfile::tempdir().expect("right");
        let files: &[(&str, &[u8])] = &[("a.txt", b"one"), ("nested/b.txt", b"two")];
        materialize_files(left.path(), files).expect("left");
        materialize_files(right.path(), files).expect("right");
        assert_eq!(
            tree_digest(left.path()).expect("left digest"),
            tree_digest(right.path()).expect("right digest")
        );
        fs::write(right.path().join("a.txt"), b"three").expect("edit");
        assert_ne!(
            tree_digest(left.path()).expect("left digest"),
            tree_digest(right.path()).expect("right digest")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_target_changes_the_dirty_digest() {
        let dir = tempfile::tempdir().expect("temp");
        let root = dir.path();
        git_repo(root, &["init"]);
        fs::write(root.join("tracked"), "base").expect("write");
        git_repo(root, &["add", "tracked"]);
        git_repo(root, &["commit", "-m", "base"]);
        std::os::unix::fs::symlink("one", root.join("link")).expect("link");
        let first = source_set_from_repo(root).expect("first").dirty_snapshot_id;
        fs::remove_file(root.join("link")).expect("unlink");
        std::os::unix::fs::symlink("two", root.join("link")).expect("relink");
        let second = source_set_from_repo(root)
            .expect("second")
            .dirty_snapshot_id;
        assert_ne!(first, second);
    }

    #[test]
    fn checkout_refuses_a_tree_that_does_not_match() {
        let origin = tempfile::tempdir().expect("origin");
        git_repo(origin.path(), &["init"]);
        fs::write(origin.path().join("tracked"), "base").expect("write");
        git_repo(origin.path(), &["add", "tracked"]);
        git_repo(origin.path(), &["commit", "-m", "base"]);
        let source = source_set_from_repo(origin.path()).expect("source");
        let dest = tempfile::tempdir().expect("dest parent");
        let checkout = dest.path().join("tree");
        checkout_source(&checkout, origin.path(), &source).expect("checkout");
        fs::write(checkout.join("extra.txt"), "dirty").expect("dirty");
        let error = verify_source_checkout(&checkout, &source).expect_err("mismatch");
        assert!(error.contains("digest"));
    }

    #[test]
    fn streamed_stdout_is_one_content_addressed_chunk() {
        let mut spec = fixture_spec("fixture-commit");
        spec.leaf = serde_json::json!({
            "kind": "native_command",
            "argv": ["/bin/echo", "wave4"]
        });
        spec.outputs.clear();
        spec.operation_id = spec.recomputed_operation_id();
        let dir = tempfile::tempdir().expect("temp");
        execute_spec_locally(&spec, dir.path(), "run_echo").expect("run");
        let journal = RunJournal::open(dir.path()).expect("journal");
        let events = journal.replay_after(0, 10).expect("events");
        let log = events
            .iter()
            .find(|event| event.event_type == "log")
            .expect("log event");
        let log_ref = log.log_ref.clone().expect("chunk");
        let bytes = journal.read_log(&log_ref).expect("chunk bytes");
        assert!(String::from_utf8_lossy(&bytes).contains("wave4"));
        assert!(log_ref.starts_with("sha256:"));
    }

    #[test]
    fn stderr_does_not_block_stdout() {
        let mut spec = fixture_spec("fixture-commit");
        spec.leaf = serde_json::json!({
            "kind": "native_command",
            "argv": [
                "/bin/bash",
                "-c",
                "dd if=/dev/zero bs=1000 count=200 status=none >&2; echo out-after-err"
            ]
        });
        spec.outputs.clear();
        spec.operation_id = spec.recomputed_operation_id();
        let dir = tempfile::tempdir().expect("temp");
        let run = execute_spec_locally(&spec, dir.path(), "run_err").expect("run");
        assert_eq!(run.exit_code, 0);
        let journal = RunJournal::open(dir.path()).expect("journal");
        let events = journal.replay_after(0, 200).expect("events");
        let stdout = events
            .iter()
            .find(|event| event.source == "stdout")
            .expect("stdout");
        let bytes = journal
            .read_log(stdout.log_ref.as_deref().expect("ref"))
            .expect("bytes");
        assert!(String::from_utf8_lossy(&bytes).contains("out-after-err"));
    }

    #[test]
    fn a_missing_declared_output_fails_the_local_run() {
        let mut spec = fixture_spec("fixture-commit");
        spec.leaf = serde_json::json!({
            "kind": "native_command",
            "argv": ["/usr/bin/true"]
        });
        spec.outputs = vec![OutputDecl {
            name: "missing.txt".to_owned(),
        }];
        spec.operation_id = spec.recomputed_operation_id();
        let dir = tempfile::tempdir().expect("temp");
        let error = execute_spec_locally(&spec, dir.path(), "run_missing").expect_err("output");
        assert!(error.contains("missing"));
    }
}
