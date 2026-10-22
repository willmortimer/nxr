//! Canonical source-set and operation identity.
//!
//! `workflow_definition_digest` hashes the workflow file only.
//! `source_set_digest` hashes a [`SourceSetV1`] manifest.
//! `operation_id` hashes every execution-affecting input. Run id, CI identity,
//! and the idempotency key are not inputs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SOURCE_SET_V1: &str = "source-set-v1";
pub const EXECUTION_SPEC_V2: &str = "execution-spec-v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmodulePin {
    pub path: String,
    pub commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockfileDigest {
    pub path: String,
    pub digest: String,
}

/// Immutable source manifest. Dirty identity is a digest, not snapshot bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSetV1 {
    pub schema_version: String,
    pub kind: String,
    pub commit: String,
    #[serde(default)]
    pub submodules: Vec<SubmodulePin>,
    #[serde(default)]
    pub lockfiles: Vec<LockfileDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_snapshot_id: Option<String>,
}

impl SourceSetV1 {
    #[must_use]
    pub fn fixture(commit: impl Into<String>) -> Self {
        Self {
            schema_version: SOURCE_SET_V1.to_owned(),
            kind: "fixture".to_owned(),
            commit: commit.into(),
            submodules: Vec::new(),
            lockfiles: Vec::new(),
            dirty_snapshot_id: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BindingDestination {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub delivery: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEnvironment {
    #[serde(default)]
    pub ordinary: BTreeMap<String, String>,
    #[serde(default)]
    pub bindings: Vec<BindingDestination>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkingDirectoryPolicy {
    pub kind: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionResources {
    pub cpu: u32,
    pub memory_bytes: u64,
    #[serde(default)]
    pub exclusive: Vec<String>,
}

/// Digest of `.nxr/workflows.toml` bytes. This is not a source-set id.
#[must_use]
pub fn workflow_definition_digest(bytes: &[u8]) -> String {
    format!("wf_{}", hex_digest(Sha256::digest(bytes).as_slice()))
}

/// Digest of the source manifest fields. A different commit changes it.
#[must_use]
pub fn source_set_digest(source: &SourceSetV1) -> String {
    let mut hasher = Sha256::new();
    push_field(&mut hasher, source.kind.as_bytes());
    push_field(&mut hasher, source.commit.as_bytes());
    let mut submodules = source.submodules.clone();
    submodules.sort_by(|left, right| left.path.cmp(&right.path));
    for pin in submodules {
        push_field(&mut hasher, pin.path.as_bytes());
        push_field(&mut hasher, pin.commit.as_bytes());
    }
    push_field(&mut hasher, b"submodules-end");
    let mut lockfiles = source.lockfiles.clone();
    lockfiles.sort_by(|left, right| left.path.cmp(&right.path));
    for lockfile in lockfiles {
        push_field(&mut hasher, lockfile.path.as_bytes());
        push_field(&mut hasher, lockfile.digest.as_bytes());
    }
    push_field(&mut hasher, b"lockfiles-end");
    push_field(
        &mut hasher,
        source.dirty_snapshot_id.as_deref().unwrap_or("").as_bytes(),
    );
    format!("src_{}", hex_digest(hasher.finalize().as_slice()))
}

/// Inputs that change what an operation does. CI metadata is absent on purpose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationIdentity {
    pub source_set_digest: String,
    pub workflow_definition_digest: String,
    pub task_id: String,
    pub leaf: Value,
    pub platform: String,
    pub architecture: String,
    pub working_directory_kind: String,
    pub working_directory_path: String,
    pub ordinary: BTreeMap<String, String>,
    pub bindings: Vec<BindingDestination>,
    pub cancel: String,
    pub jobs: u32,
    pub timeout_ms: String,
    pub cpu: u32,
    pub memory_bytes: u64,
    pub exclusive: Vec<String>,
    pub outputs: Vec<String>,
}

/// `op_` plus 32 hex characters. Independent of run id and idempotency key.
#[must_use]
pub fn operation_id(identity: &OperationIdentity) -> String {
    let mut hasher = Sha256::new();
    push_field(&mut hasher, identity.source_set_digest.as_bytes());
    push_field(&mut hasher, identity.workflow_definition_digest.as_bytes());
    push_field(&mut hasher, identity.task_id.as_bytes());
    push_field(&mut hasher, canonical_json(&identity.leaf).as_bytes());
    push_field(&mut hasher, identity.platform.as_bytes());
    push_field(&mut hasher, identity.architecture.as_bytes());
    push_field(&mut hasher, identity.working_directory_kind.as_bytes());
    push_field(&mut hasher, identity.working_directory_path.as_bytes());
    for (key, value) in &identity.ordinary {
        push_field(&mut hasher, key.as_bytes());
        push_field(&mut hasher, value.as_bytes());
    }
    push_field(&mut hasher, b"ordinary-end");
    let mut bindings = identity.bindings.clone();
    bindings.sort_by(|left, right| left.name.cmp(&right.name));
    for binding in bindings {
        push_field(&mut hasher, binding.name.as_bytes());
        push_field(&mut hasher, binding.reference.as_bytes());
        push_field(&mut hasher, binding.delivery.as_bytes());
    }
    push_field(&mut hasher, b"bindings-end");
    push_field(&mut hasher, identity.cancel.as_bytes());
    push_field(&mut hasher, identity.jobs.to_string().as_bytes());
    push_field(&mut hasher, identity.timeout_ms.as_bytes());
    push_field(&mut hasher, identity.cpu.to_string().as_bytes());
    push_field(&mut hasher, identity.memory_bytes.to_string().as_bytes());
    let mut exclusive = identity.exclusive.clone();
    exclusive.sort();
    push_field(&mut hasher, exclusive.join("\n").as_bytes());
    push_field(&mut hasher, b"exclusive-end");
    let mut outputs = identity.outputs.clone();
    outputs.sort();
    push_field(&mut hasher, outputs.join("\n").as_bytes());
    push_field(&mut hasher, b"outputs-end");
    let digest = hasher.finalize();
    let mut id = String::from("op_");
    for byte in digest.iter().take(16) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// One dirty-manifest entry after file bytes have been hashed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirtySnapshotEntry {
    pub path: String,
    pub mode: String,
    pub kind: String,
    pub payload: String,
}

/// Digest of a dirty manifest. This is the `dirty_snapshot_id` inside a source set.
#[must_use]
pub fn dirty_snapshot_digest(entries: &[DirtySnapshotEntry]) -> String {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for entry in ordered {
        for field in [entry.path, entry.mode, entry.kind, entry.payload] {
            push_field(&mut hasher, field.as_bytes());
        }
    }
    format!("dirty_{}", hex_digest(hasher.finalize().as_slice()))
}

pub(crate) fn push_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Object keys are sorted so both repositories hash the same leaf bytes.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(&map[key])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_owned()),
    }
}
