//! Artifact availability. The scheduler may skip a node only when every
//! answer is yes. A missing answer is `not_measured`, which does not skip.

use serde::{Deserialize, Serialize};

/// One of the four reuse questions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The answer is yes.
    Yes,
    /// The answer is no.
    No,
    /// Nobody measured this.
    NotMeasured,
}

/// Whether a stored artifact can stand in for a new execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseDecision {
    /// Run the node.
    Execute,
    /// The stored artifact satisfies this consumer.
    Skip,
}

/// The four questions, kept separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactAvailability {
    /// Schema name.
    pub schema: String,
    /// A compatible artifact was produced by an eligible attempt.
    pub produced: Availability,
    /// The stored digest was verified.
    pub digest_verified: Availability,
    /// The intended consumer can read it.
    pub accessible: Availability,
    /// Platform, toolchain, source, and trust match.
    pub requirements_match: Availability,
}

impl ArtifactAvailability {
    /// Every question starts unmeasured.
    #[must_use]
    pub fn not_measured() -> Self {
        Self {
            schema: "artifact-availability-v1".to_owned(),
            produced: Availability::NotMeasured,
            digest_verified: Availability::NotMeasured,
            accessible: Availability::NotMeasured,
            requirements_match: Availability::NotMeasured,
        }
    }
}

/// What the current execution asks for. These values do not come from the receipt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentRequirements {
    /// Intended consumer.
    pub consumer: String,
    /// Toolchain the task requires now.
    pub toolchain: String,
    /// Platform the task requires now.
    pub platform: String,
    /// Source digest the task requires now.
    pub source_digest: String,
}

/// A reuse the scheduler accepted. The event stream records this separately from success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReuse {
    /// Task id.
    pub node: String,
    /// Verified content digest.
    pub digest: String,
    /// Producer attempt that wrote the receipt.
    pub producer_attempt: String,
    /// Where the bytes were read.
    pub storage_layer: String,
    /// Producer receipt that was checked against the current plan and the object bytes.
    pub verification_receipt: String,
}

/// Resolve reuse from the current requirements, a producer receipt, and the object bytes.
///
/// The receipt is `.nxr/artifacts/receipts/{node}.json`. It is not allowed to
/// supply the expected consumer, toolchain, platform, or source. A missing
/// input is `not_measured`.
#[must_use]
pub fn resolve_availability(
    root: &std::path::Path,
    node: &str,
    requirements: &CurrentRequirements,
) -> (ArtifactAvailability, Option<ArtifactReuse>) {
    let path = root
        .join(".nxr")
        .join("artifacts")
        .join("receipts")
        .join(format!("{node}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (ArtifactAvailability::not_measured(), None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (ArtifactAvailability::not_measured(), None);
    };
    let attempt = value
        .get("producerAttempt")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let outcome = value.get("outcome").and_then(|item| item.as_str());
    let produced = match (attempt.is_empty(), outcome) {
        (false, Some("succeeded")) => Availability::Yes,
        (_, Some(_)) => Availability::No,
        _ => Availability::NotMeasured,
    };
    let claimed = value
        .get("contentDigest")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let location = value.get("location").and_then(|item| item.as_str());
    let (digest_verified, accessible) = match location {
        Some(location) => match std::fs::read(location) {
            Ok(bytes) => {
                let actual = format!("sha256:{}", hex_sha256(&bytes));
                let verified = if claimed.is_empty() {
                    Availability::NotMeasured
                } else if actual.eq_ignore_ascii_case(claimed) {
                    Availability::Yes
                } else {
                    Availability::No
                };
                (verified, Availability::Yes)
            }
            Err(_) => (Availability::NotMeasured, Availability::No),
        },
        None => (Availability::NotMeasured, Availability::NotMeasured),
    };
    let requirements_match = compare_requirement(
        value.get("consumer").and_then(|item| item.as_str()),
        &requirements.consumer,
    )
    .and_then(|consumer| {
        compare_requirement(
            value.get("toolchain").and_then(|item| item.as_str()),
            &requirements.toolchain,
        )
        .map(|toolchain| consumer && toolchain)
    })
    .and_then(|so_far| {
        compare_requirement(
            value.get("platform").and_then(|item| item.as_str()),
            &requirements.platform,
        )
        .map(|platform| so_far && platform)
    })
    .and_then(|so_far| {
        compare_requirement(
            value.get("source").and_then(|item| item.as_str()),
            &requirements.source_digest,
        )
        .map(|source| so_far && source)
    })
    .map(|matched| {
        if matched {
            Availability::Yes
        } else {
            Availability::No
        }
    })
    .unwrap_or(Availability::NotMeasured);
    let availability = ArtifactAvailability {
        schema: "artifact-availability-v1".to_owned(),
        produced,
        digest_verified,
        accessible,
        requirements_match,
    };
    let reuse = if !claimed.is_empty() && !attempt.is_empty() {
        Some(ArtifactReuse {
            node: node.to_owned(),
            digest: claimed.to_owned(),
            producer_attempt: attempt.to_owned(),
            storage_layer: "local_file".to_owned(),
            verification_receipt: path.display().to_string(),
        })
    } else {
        None
    };
    (availability, reuse)
}

fn compare_requirement(recorded: Option<&str>, current: &str) -> Option<bool> {
    let recorded = recorded.filter(|text| !text.is_empty())?;
    if current.is_empty() {
        return None;
    }
    Some(recorded == current)
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Skip only when all four answers are yes.
#[must_use]
pub fn reuse_decision(record: &ArtifactAvailability) -> ReuseDecision {
    if record.produced == Availability::Yes
        && record.digest_verified == Availability::Yes
        && record.accessible == Availability::Yes
        && record.requirements_match == Availability::Yes
    {
        ReuseDecision::Skip
    } else {
        ReuseDecision::Execute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warm() -> ArtifactAvailability {
        ArtifactAvailability {
            schema: "artifact-availability-v1".into(),
            produced: Availability::Yes,
            digest_verified: Availability::Yes,
            accessible: Availability::Yes,
            requirements_match: Availability::Yes,
        }
    }

    #[test]
    fn stargate_cases_stay_separate() {
        assert_eq!(reuse_decision(&warm()), ReuseDecision::Skip);
        let mut cold = warm();
        cold.produced = Availability::No;
        assert_eq!(reuse_decision(&cold), ReuseDecision::Execute);
        let mut changed = warm();
        changed.requirements_match = Availability::No;
        assert_eq!(reuse_decision(&changed), ReuseDecision::Execute);
        let mut corrupt = warm();
        corrupt.digest_verified = Availability::No;
        assert_eq!(reuse_decision(&corrupt), ReuseDecision::Execute);
        let mut mismatch = warm();
        mismatch.accessible = Availability::No;
        assert_eq!(reuse_decision(&mismatch), ReuseDecision::Execute);
        assert_eq!(
            reuse_decision(&ArtifactAvailability::not_measured()),
            ReuseDecision::Execute
        );
    }

    #[test]
    fn a_record_is_not_measured_until_every_answer_is_present() {
        let root = tempfile::tempdir().unwrap();
        let receipts = root.path().join(".nxr/artifacts/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        let location = root.path().join("mod.wasm");
        let bytes = b"wasm-bytes";
        std::fs::write(&location, bytes).unwrap();
        let digest = format!("sha256:{}", hex_sha256(bytes));
        let body = serde_json::json!({
            "contentDigest": digest,
            "producerAttempt": "att",
            "outcome": "succeeded",
            "location": location,
            "consumer": "worker-a",
            "toolchain": "wasm-bindgen",
            "platform": "wasm32",
            "source": "tree"
        });
        std::fs::write(receipts.join("stargate.json"), body.to_string()).unwrap();
        let changed = CurrentRequirements {
            consumer: "worker-a".into(),
            toolchain: "wasm-bindgen".into(),
            platform: "wasm32".into(),
            source_digest: "changed".into(),
        };
        let (availability, _) = resolve_availability(root.path(), "stargate", &changed);
        assert_eq!(availability.produced, Availability::Yes);
        assert_eq!(availability.digest_verified, Availability::Yes);
        assert_eq!(availability.accessible, Availability::Yes);
        assert_eq!(availability.requirements_match, Availability::No);
        assert_eq!(reuse_decision(&availability), ReuseDecision::Execute);
        std::fs::write(&location, b"corrupt").unwrap();
        let (corrupt, _) = resolve_availability(root.path(), "stargate", &changed);
        assert_eq!(corrupt.digest_verified, Availability::No);
        assert_eq!(
            resolve_availability(root.path(), "missing", &changed)
                .0
                .produced,
            Availability::NotMeasured
        );
    }
}
