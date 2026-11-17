//! Child-observed toolchain provenance and attempt receipts.
//!
//! Probe fields come from what the child printed. This module never reads the
//! parent process environment and labels it as the child's.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Toolchain identity the child itself printed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChildObservation {
    pub rustc_path: String,
    pub rustc_version: String,
    pub cargo_path: String,
    pub cargo_version: String,
    pub wrapper_path: String,
    pub linker_path: String,
    pub target_directory: String,
    pub cache_identity: String,
}

const LEGAL_STATES: &[&str] = &[
    "planned",
    "admitted",
    "started",
    "environment-verified",
    "completed",
    "artifact-verified",
    "qualified",
];

/// POSIX `sh` snippet that prints `key=value` lines from the child environment.
#[must_use]
pub fn child_probe_script() -> &'static str {
    r#"rustc_path=$(command -v rustc) || exit 1
rustc_version=$("$rustc_path" --version) || exit 1
cargo_path=$(command -v cargo) || exit 1
cargo_version=$("$cargo_path" --version) || exit 1
if [ -n "${RUSTC_WRAPPER:-}" ]; then
  wrapper_path=$(command -v "$RUSTC_WRAPPER") || wrapper_path=
else
  wrapper_path=
fi
linker_path=$(command -v cc) || linker_path=
target_directory=${CARGO_TARGET_DIR:-}
cache_identity=${NXR_CACHE_IDENTITY:-}
printf '%s\n' \
  "rustc_path=${rustc_path}" \
  "rustc_version=${rustc_version}" \
  "cargo_path=${cargo_path}" \
  "cargo_version=${cargo_version}" \
  "wrapper_path=${wrapper_path}" \
  "linker_path=${linker_path}" \
  "target_directory=${target_directory}" \
  "cache_identity=${cache_identity}"
"#
}

/// Parse child-printed `key=value` probe output.
///
/// # Errors
///
/// Returns an error when required rustc/cargo fields are missing or empty.
pub fn parse_child_probe(text: &str) -> Result<ChildObservation, String> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("invalid probe line: {line}"));
        };
        fields.insert(key.to_owned(), value.to_owned());
    }

    let require = |key: &str| -> Result<String, String> {
        fields
            .get(key)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| format!("missing probe field: {key}"))
    };
    let optional = |key: &str| -> String { fields.get(key).cloned().unwrap_or_default() };

    Ok(ChildObservation {
        rustc_path: require("rustc_path")?,
        rustc_version: require("rustc_version")?,
        cargo_path: require("cargo_path")?,
        cargo_version: require("cargo_version")?,
        wrapper_path: optional("wrapper_path"),
        linker_path: optional("linker_path"),
        target_directory: optional("target_directory"),
        cache_identity: optional("cache_identity"),
    })
}

/// Durable receipt for one run attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredReceipt {
    pub state: String,
    pub operation_id: String,
    pub workspace_id: String,
    pub run_id: String,
    pub source_digest: String,
    /// Empty for a run-level receipt written before leaf ids existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub leaf_id: String,
    pub attempt: u32,
    pub observation: Option<ChildObservation>,
}

fn receipt_filename(run_id: &str, leaf_id: &str, attempt: u32) -> String {
    if leaf_id.is_empty() {
        format!("{run_id}.attempt-{attempt}.json")
    } else {
        format!("{run_id}.{leaf_id}.attempt-{attempt}.json")
    }
}

fn receipt_path(dir: &Path, run_id: &str, leaf_id: &str, attempt: u32) -> PathBuf {
    dir.join(receipt_filename(run_id, leaf_id, attempt))
}

/// Write `{run_id}.attempt-{attempt}.json`. Refuses to replace an existing file.
///
/// # Errors
///
/// Returns an error when the directory cannot be created, the file already
/// exists, or serialization/write fails.
pub fn write_receipt(dir: &Path, receipt: &StoredReceipt) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("create receipt dir: {err}"))?;
    let path = receipt_path(dir, &receipt.run_id, &receipt.leaf_id, receipt.attempt);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| format!("create receipt {}: {err}", path.display()))?;
    let body =
        serde_json::to_vec_pretty(receipt).map_err(|err| format!("serialize receipt: {err}"))?;
    file.write_all(&body)
        .map_err(|err| format!("write receipt {}: {err}", path.display()))?;
    Ok(())
}

/// Read a previously written attempt receipt.
///
/// # Errors
///
/// Returns an error when the file is missing or cannot be parsed.
pub fn read_receipt(dir: &Path, run_id: &str, attempt: u32) -> Result<StoredReceipt, String> {
    read_leaf_receipt(dir, run_id, "", attempt)
}

/// Read the receipt for one leaf. An empty `leaf_id` is the whole command.
///
/// # Errors
///
/// Returns an error when the file is missing or cannot be parsed.
pub fn read_leaf_receipt(
    dir: &Path,
    run_id: &str,
    leaf_id: &str,
    attempt: u32,
) -> Result<StoredReceipt, String> {
    let path = receipt_path(dir, run_id, leaf_id, attempt);
    let body = fs::read(&path).map_err(|err| format!("read receipt {}: {err}", path.display()))?;
    serde_json::from_slice(&body).map_err(|err| format!("parse receipt {}: {err}", path.display()))
}

fn next_legal_state(current: &str) -> Result<&'static str, String> {
    let Some(index) = LEGAL_STATES.iter().position(|state| *state == current) else {
        return Err(format!(
            "illegal receipt transition: unknown state {current}"
        ));
    };
    LEGAL_STATES
        .get(index + 1)
        .copied()
        .ok_or_else(|| format!("illegal receipt transition: {current} is terminal"))
}

/// Advance one legal hop, or fail with `illegal receipt transition`.
///
/// `environment-verified` requires `observation` to be `Some`.
///
/// # Errors
///
/// Returns an error for illegal jumps or a missing observation when verifying.
pub fn transition(receipt: &StoredReceipt, next: &str) -> Result<StoredReceipt, String> {
    let expected = next_legal_state(&receipt.state)?;
    if next != expected {
        return Err(format!(
            "illegal receipt transition from {} to {next}",
            receipt.state
        ));
    }
    if next == "environment-verified" && receipt.observation.is_none() {
        return Err("environment-verified requires observation".to_owned());
    }
    Ok(StoredReceipt {
        state: next.to_owned(),
        observation: receipt.observation.clone(),
        operation_id: receipt.operation_id.clone(),
        workspace_id: receipt.workspace_id.clone(),
        run_id: receipt.run_id.clone(),
        source_digest: receipt.source_digest.clone(),
        leaf_id: receipt.leaf_id.clone(),
        attempt: receipt.attempt,
    })
}

/// Build the next attempt in memory: `planned`, cleared observation, attempt+1.
#[must_use]
pub fn next_attempt(previous: &StoredReceipt) -> StoredReceipt {
    StoredReceipt {
        state: "planned".to_owned(),
        operation_id: previous.operation_id.clone(),
        workspace_id: previous.workspace_id.clone(),
        run_id: previous.run_id.clone(),
        source_digest: previous.source_digest.clone(),
        leaf_id: previous.leaf_id.clone(),
        attempt: previous.attempt.saturating_add(1),
        observation: None,
    }
}

fn validate_artifact_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if path.is_absolute() || name.contains("..") || name.contains('/') {
        return Err(format!("illegal artifact name: {name}"));
    }
    Ok(())
}

fn artifact_path(root: &Path, run_id: &str, name: &str) -> PathBuf {
    root.join(run_id).join(name)
}

/// Store artifact bytes under `root/run_id/name` and return their digest.
///
/// # Errors
///
/// Returns an error for illegal names or I/O failures.
pub fn put_artifact(root: &Path, run_id: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
    validate_artifact_name(name)?;
    let run_dir = root.join(run_id);
    fs::create_dir_all(&run_dir).map_err(|err| format!("create artifact run dir: {err}"))?;
    let path = artifact_path(root, run_id, name);
    fs::write(&path, bytes).map_err(|err| format!("write artifact {}: {err}", path.display()))?;
    Ok(nxr_core::digest_bytes(bytes))
}

/// Read an artifact and require the stored digest to match `expected_digest`.
///
/// # Errors
///
/// Returns an error when the name is illegal, the path is a symlink, the file
/// is missing, or the digest differs.
pub fn get_artifact(
    root: &Path,
    run_id: &str,
    name: &str,
    expected_digest: &str,
) -> Result<Vec<u8>, String> {
    validate_artifact_name(name)?;
    let path = artifact_path(root, run_id, name);
    let meta = fs::symlink_metadata(&path)
        .map_err(|err| format!("stat artifact {}: {err}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("artifact path is a symlink: {}", path.display()));
    }
    let bytes =
        fs::read(&path).map_err(|err| format!("read artifact {}: {err}", path.display()))?;
    let digest = nxr_core::digest_bytes(&bytes);
    if digest != expected_digest {
        return Err(format!(
            "artifact digest mismatch: expected {expected_digest}, got {digest}"
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_observation() -> ChildObservation {
        ChildObservation {
            rustc_path: "/nix/store/rustc".to_owned(),
            rustc_version: "rustc 1.80.0".to_owned(),
            cargo_path: "/nix/store/cargo".to_owned(),
            cargo_version: "cargo 1.80.0".to_owned(),
            wrapper_path: String::new(),
            linker_path: "/usr/bin/cc".to_owned(),
            target_directory: String::new(),
            cache_identity: String::new(),
        }
    }

    fn sample_receipt() -> StoredReceipt {
        StoredReceipt {
            state: "planned".to_owned(),
            operation_id: "op_test".to_owned(),
            workspace_id: "ws_test".to_owned(),
            run_id: "run_test".to_owned(),
            source_digest: "digest_src".to_owned(),
            leaf_id: String::new(),
            attempt: 1,
            observation: None,
        }
    }

    #[test]
    fn parse_fixture_probe_with_empty_wrapper() {
        let text = "\
rustc_path=/nix/store/rustc
rustc_version=rustc 1.80.0
cargo_path=/nix/store/cargo
cargo_version=cargo 1.80.0
wrapper_path=
linker_path=/usr/bin/cc
target_directory=
cache_identity=
";
        let observed = parse_child_probe(text).expect("parse");
        assert_eq!(observed, sample_observation());
        assert!(observed.wrapper_path.is_empty());
    }

    #[test]
    fn planned_to_admitted_succeeds_and_planned_to_qualified_fails() {
        let planned = sample_receipt();
        let admitted = transition(&planned, "admitted").expect("admitted");
        assert_eq!(admitted.state, "admitted");
        let err = transition(&planned, "qualified").expect_err("skip");
        assert!(
            err.contains("illegal receipt transition"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = sample_receipt();
        write_receipt(dir.path(), &receipt).expect("write");
        let restored = read_receipt(dir.path(), &receipt.run_id, receipt.attempt).expect("read");
        assert_eq!(restored, receipt);
    }

    #[test]
    fn second_write_same_attempt_fails_and_first_file_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut receipt = sample_receipt();
        write_receipt(dir.path(), &receipt).expect("first write");
        let path = receipt_path(
            dir.path(),
            &receipt.run_id,
            &receipt.leaf_id,
            receipt.attempt,
        );
        let original = fs::read(&path).expect("original bytes");
        receipt.state = "admitted".to_owned();
        assert!(write_receipt(dir.path(), &receipt).is_err());
        assert_eq!(fs::read(&path).expect("reread"), original);
    }

    #[test]
    fn next_attempt_increments_and_does_not_delete_first_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut first = sample_receipt();
        first.observation = Some(sample_observation());
        first.state = "completed".to_owned();
        write_receipt(dir.path(), &first).expect("write first");
        let path = receipt_path(dir.path(), &first.run_id, &first.leaf_id, first.attempt);
        assert!(path.is_file());
        let next = next_attempt(&first);
        assert_eq!(next.attempt, first.attempt + 1);
        assert_eq!(next.state, "planned");
        assert!(next.observation.is_none());
        assert_eq!(next.operation_id, first.operation_id);
        assert_eq!(next.workspace_id, first.workspace_id);
        assert_eq!(next.run_id, first.run_id);
        assert_eq!(next.source_digest, first.source_digest);
        assert!(path.is_file());
    }

    #[test]
    fn put_and_get_artifact_round_trip() {
        let root = tempfile::tempdir().expect("tempdir");
        let digest = put_artifact(root.path(), "run_a", "out.bin", b"payload").expect("put");
        let bytes = get_artifact(root.path(), "run_a", "out.bin", &digest).expect("get");
        assert_eq!(bytes, b"payload");
    }

    #[test]
    fn wrong_digest_fails() {
        let root = tempfile::tempdir().expect("tempdir");
        let _digest = put_artifact(root.path(), "run_a", "out.bin", b"payload").expect("put");
        let err =
            get_artifact(root.path(), "run_a", "out.bin", "wrong").expect_err("digest mismatch");
        assert!(err.contains("digest"), "unexpected error: {err}");
    }

    #[test]
    fn name_dotdot_secret_fails() {
        let root = tempfile::tempdir().expect("tempdir");
        let err = put_artifact(root.path(), "run_a", "../secret", b"x").expect_err("reject");
        assert!(
            err.contains("..") || err.contains("illegal"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn another_run_id_cannot_read_artifact() {
        let root = tempfile::tempdir().expect("tempdir");
        let digest = put_artifact(root.path(), "run_a", "out.bin", b"payload").expect("put");
        let err =
            get_artifact(root.path(), "run_b", "out.bin", &digest).expect_err("other run missing");
        assert!(!err.is_empty());
    }
}
