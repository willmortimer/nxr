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
pub const SOURCE_SET_V2: &str = "source-set-v2";
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
pub struct SourceMemberV2 {
    pub name: String,
    pub logical_path: String,
    pub kind: String,
    pub commit: String,
    #[serde(default)]
    pub submodules: Vec<SubmodulePin>,
    #[serde(default)]
    pub lockfiles: Vec<LockfileDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_snapshot_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSetV2 {
    pub schema_version: String,
    pub primary: String,
    pub members: Vec<SourceMemberV2>,
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

/// Validate a multi-member source set document.
///
/// # Errors
///
/// Returns an error when primary is empty or missing, a member name is empty or
/// duplicated, or a logical path is empty, absolute, contains `..`, or contains `:`.
pub fn source_set_v2(set: &SourceSetV2) -> Result<(), String> {
    if set.primary.is_empty() {
        return Err("primary is empty".to_owned());
    }
    let mut seen_names = std::collections::BTreeSet::new();
    let mut primary_found = false;
    for member in &set.members {
        if member.name.is_empty() {
            return Err("member name is empty".to_owned());
        }
        if !seen_names.insert(member.name.as_str()) {
            return Err(format!("duplicate member name: {}", member.name));
        }
        if member.name == set.primary {
            primary_found = true;
        }
        validate_logical_path(&member.logical_path)?;
    }
    if !primary_found {
        return Err(format!(
            "primary `{}` is not a member name",
            set.primary
        ));
    }
    Ok(())
}

fn validate_logical_path(logical_path: &str) -> Result<(), String> {
    if logical_path.is_empty() {
        return Err("logical_path is empty".to_owned());
    }
    if logical_path.starts_with('/') || logical_path.starts_with('\\') {
        return Err(format!("logical_path `{logical_path}` must not be absolute"));
    }
    if logical_path.contains("..") {
        return Err(format!("logical_path `{logical_path}` must not contain `..`"));
    }
    if logical_path.contains(':') {
        return Err(format!("logical_path `{logical_path}` must not contain `:`"));
    }
    Ok(())
}

/// Digest of a multi-member source set. Members are sorted by name.
///
/// # Errors
///
/// Returns the same validation errors as [`source_set_v2`].
pub fn source_set_digest_v2(set: &SourceSetV2) -> Result<String, String> {
    source_set_v2(set)?;
    let mut hasher = Sha256::new();
    push_field(&mut hasher, SOURCE_SET_V2.as_bytes());
    push_field(&mut hasher, set.primary.as_bytes());
    let mut members = set.members.clone();
    members.sort_by(|left, right| left.name.cmp(&right.name));
    for member in members {
        push_field(&mut hasher, member.name.as_bytes());
        push_field(&mut hasher, member.logical_path.as_bytes());
        push_field(&mut hasher, member.kind.as_bytes());
        push_field(&mut hasher, member.commit.as_bytes());
        let mut submodules = member.submodules.clone();
        submodules.sort_by(|left, right| left.path.cmp(&right.path));
        for pin in submodules {
            push_field(&mut hasher, pin.path.as_bytes());
            push_field(&mut hasher, pin.commit.as_bytes());
        }
        push_field(&mut hasher, b"submodules-end");
        let mut lockfiles = member.lockfiles.clone();
        lockfiles.sort_by(|left, right| left.path.cmp(&right.path));
        for lockfile in lockfiles {
            push_field(&mut hasher, lockfile.path.as_bytes());
            push_field(&mut hasher, lockfile.digest.as_bytes());
        }
        push_field(&mut hasher, b"lockfiles-end");
        push_field(
            &mut hasher,
            member.dirty_snapshot_id.as_deref().unwrap_or("").as_bytes(),
        );
    }
    push_field(&mut hasher, b"members-end");
    Ok(format!("src2_{}", hex_digest(hasher.finalize().as_slice())))
}

/// Project the primary member into a v1 source set for v1 Hub compatibility.
///
/// # Errors
///
/// Returns the same validation errors as [`source_set_v2`].
pub fn project_primary_v1(set: &SourceSetV2) -> Result<SourceSetV1, String> {
    source_set_v2(set)?;
    let primary = set
        .members
        .iter()
        .find(|member| member.name == set.primary)
        .expect("validated primary is a member");
    Ok(SourceSetV1 {
        schema_version: SOURCE_SET_V1.to_owned(),
        kind: primary.kind.clone(),
        commit: primary.commit.clone(),
        submodules: primary.submodules.clone(),
        lockfiles: primary.lockfiles.clone(),
        dirty_snapshot_id: primary.dirty_snapshot_id.clone(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lattice_stargate_example(stargate_commit: &str) -> SourceSetV2 {
        SourceSetV2 {
            schema_version: SOURCE_SET_V2.to_owned(),
            primary: "lattice".to_owned(),
            members: vec![
                SourceMemberV2 {
                    name: "lattice".to_owned(),
                    logical_path: "lattice".to_owned(),
                    kind: "git".to_owned(),
                    commit: "aaa".to_owned(),
                    submodules: Vec::new(),
                    lockfiles: Vec::new(),
                    dirty_snapshot_id: None,
                },
                SourceMemberV2 {
                    name: "stargate".to_owned(),
                    logical_path: "deps/stargate".to_owned(),
                    kind: "git".to_owned(),
                    commit: stargate_commit.to_owned(),
                    submodules: Vec::new(),
                    lockfiles: Vec::new(),
                    dirty_snapshot_id: None,
                },
            ],
        }
    }

    #[test]
    fn source_set_v1_golden_fixture_digest() {
        let digest = source_set_digest(&SourceSetV1::fixture("c1-golden"));
        assert_eq!(
            digest,
            "src_a47ed7bd8e42c2bc7a4b1911d23f66c805f9aae28fcbe4871df8cb23f9c4ce87"
        );
    }

    #[test]
    fn source_set_v2_lattice_stargate_digest() {
        let set = lattice_stargate_example("bbb");
        assert_eq!(
            source_set_digest_v2(&set).expect("valid"),
            "src2_501f07df541ef192dad7831f68372b7ee9105a55fe1f0c2657913f618783ab7f"
        );
    }

    #[test]
    fn source_set_v2_member_order_does_not_change_digest() {
        let mut set = lattice_stargate_example("bbb");
        set.members.reverse();
        assert_eq!(
            source_set_digest_v2(&set).expect("valid"),
            "src2_501f07df541ef192dad7831f68372b7ee9105a55fe1f0c2657913f618783ab7f"
        );
    }

    #[test]
    fn source_set_v2_stargate_commit_ccc_digest() {
        let set = lattice_stargate_example("ccc");
        assert_eq!(
            source_set_digest_v2(&set).expect("valid"),
            "src2_2386048beda35c5fc6ca878a72f1851a2450218b6ff41d2624bb5c609630b724"
        );
    }

    #[test]
    fn source_set_v2_dirty_snapshot_changes_digest() {
        let clean = lattice_stargate_example("bbb");
        let clean_digest = source_set_digest_v2(&clean).expect("valid");
        let mut dirty = lattice_stargate_example("bbb");
        dirty.members[1].dirty_snapshot_id = Some("dirty_example".to_owned());
        let dirty_digest = source_set_digest_v2(&dirty).expect("valid");
        assert_ne!(clean_digest, dirty_digest);
        assert_ne!(
            dirty_digest,
            "src2_501f07df541ef192dad7831f68372b7ee9105a55fe1f0c2657913f618783ab7f"
        );
        assert_ne!(
            dirty_digest,
            "src2_2386048beda35c5fc6ca878a72f1851a2450218b6ff41d2624bb5c609630b724"
        );
    }

    #[test]
    fn source_set_v2_rejects_duplicate_names_and_missing_primary() {
        let mut duplicate = lattice_stargate_example("bbb");
        duplicate.members[1].name = "lattice".to_owned();
        assert!(source_set_v2(&duplicate).is_err());

        let mut missing_primary = lattice_stargate_example("bbb");
        missing_primary.primary = "missing".to_owned();
        assert!(source_set_v2(&missing_primary).is_err());
    }

    #[test]
    fn project_primary_v1_copies_primary_without_v2_digest() {
        let set = lattice_stargate_example("bbb");
        let projected = project_primary_v1(&set).expect("valid");
        assert_eq!(projected.commit, "aaa");
        assert_eq!(projected.schema_version, SOURCE_SET_V1);
        let v1_digest = source_set_digest(&projected);
        let v2_digest = source_set_digest_v2(&set).expect("valid");
        assert!(!v1_digest.starts_with("src2_"));
        assert_ne!(v1_digest, v2_digest);
    }
}
