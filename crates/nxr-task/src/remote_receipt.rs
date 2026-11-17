//! Source-bound receipt for one remote attempt.
//!
//! An artifact digest is not a test qualification. Source reuse and an
//! existing artifact stay separate labels.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteReceipt {
    pub source_set_id: String,
    pub operation_id: String,
    pub run_id: String,
    pub attempt_id: String,
    pub lease_generation: Option<u64>,
    pub output_digest: String,
    pub source_disposition: String,
    pub execution_disposition: String,
    pub artifact_verified: bool,
}

/// Classify journal text without turning a cache hit into a qualification.
#[must_use]
pub fn dispositions_from_journal(journal: &Value) -> (String, String) {
    let text = journal.to_string();
    let source =
        if text.contains("\"source_reused\":true") || text.contains("\"source\":\"reused\"") {
            "reused"
        } else {
            "fetched"
        };
    let execution =
        if text.contains("\"existing_artifact\":true") || text.contains("\"cache_hit\":true") {
            "existing_artifact"
        } else {
            "fresh"
        };
    (source.to_owned(), execution.to_owned())
}

/// Check `bytes` against `expected`, which may be hex or `sha256:` hex.
///
/// # Errors
///
/// Returns an error when the digest does not match.
pub fn verify_artifact(bytes: &[u8], expected: &str) -> Result<String, String> {
    let actual = hex_sha256(bytes);
    let wanted = expected.trim().trim_start_matches("sha256:");
    if !wanted.eq_ignore_ascii_case(&actual) {
        return Err(format!(
            "artifact digest {actual} does not match {expected}"
        ));
    }
    Ok(format!("sha256:{actual}"))
}

/// Write the receipt next to the run. A verified artifact stays distinct from a passing test.
///
/// # Errors
///
/// Returns an error when the file cannot be written or a label is unknown.
pub fn store_remote_receipt(
    dir: &Path,
    receipt: &RemoteReceipt,
) -> Result<std::path::PathBuf, String> {
    if receipt.source_disposition != "fetched" && receipt.source_disposition != "reused" {
        return Err("source disposition is unknown".to_owned());
    }
    if receipt.execution_disposition != "fresh"
        && receipt.execution_disposition != "existing_artifact"
    {
        return Err("execution disposition is unknown".to_owned());
    }
    if !receipt.artifact_verified {
        return Err("artifact is not verified".to_owned());
    }
    let path = dir
        .join("provenance")
        .join(format!("{}.remote.json", receipt.run_id));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create receipt dir: {err}"))?;
    }
    let text = serde_json::to_string_pretty(receipt).map_err(|err| format!("receipt: {err}"))?;
    fs::write(&path, text).map_err(|err| format!("write receipt: {err}"))?;
    Ok(path)
}

/// Read a stored remote receipt.
///
/// # Errors
///
/// Returns an error when the file exists and cannot be parsed.
pub fn read_remote_receipt(dir: &Path, run_id: &str) -> Result<Option<RemoteReceipt>, String> {
    let path = dir.join("provenance").join(format!("{run_id}.remote.json"));
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| format!("receipt: {err}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read receipt: {err}")),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_changed_byte_fails_digest_verification() {
        let digest = verify_artifact(b"hi", &format!("sha256:{}", hex_sha256(b"hi"))).expect("ok");
        assert!(digest.starts_with("sha256:"));
        let error = verify_artifact(b"no", &digest).expect_err("mismatch");
        assert!(error.contains("does not match"));
    }

    #[test]
    fn cache_labels_stay_distinct_from_a_fresh_fetch() {
        let (source, execution) = dispositions_from_journal(&json!([
            {"source_reused": true},
            {"existing_artifact": true}
        ]));
        assert_eq!(source, "reused");
        assert_eq!(execution, "existing_artifact");
        let (fetched, fresh) = dispositions_from_journal(&json!([{"event_type": "exited"}]));
        assert_eq!(fetched, "fetched");
        assert_eq!(fresh, "fresh");
    }

    #[test]
    fn the_receipt_records_the_attempt_and_the_output_digest() {
        let dir = tempfile::tempdir().expect("temp");
        let digest = verify_artifact(b"marker", &hex_sha256(b"marker")).expect("digest");
        let receipt = RemoteReceipt {
            source_set_id: "src2_a".to_owned(),
            operation_id: "op_1".to_owned(),
            run_id: "run-once".to_owned(),
            attempt_id: "att_1".to_owned(),
            lease_generation: Some(2),
            output_digest: digest,
            source_disposition: "fetched".to_owned(),
            execution_disposition: "fresh".to_owned(),
            artifact_verified: true,
        };
        store_remote_receipt(dir.path(), &receipt).expect("store");
        let stored = read_remote_receipt(dir.path(), "run-once")
            .expect("read")
            .expect("present");
        assert_eq!(stored, receipt);
        assert_ne!(stored.execution_disposition, "qualified");
    }
}
