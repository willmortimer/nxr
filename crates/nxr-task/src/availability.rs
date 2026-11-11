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

/// Derive the four answers from a stored artifact record.
///
/// A missing field is `not_measured`. A mismatch is `no`. The scheduler skips
/// only when [`reuse_decision`] returns [`ReuseDecision::Skip`].
#[must_use]
pub fn resolve_availability(root: &std::path::Path, node: &str) -> ArtifactAvailability {
    let path = root
        .join(".nxr")
        .join("artifacts")
        .join("records")
        .join(format!("{node}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return ArtifactAvailability::not_measured();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ArtifactAvailability::not_measured();
    };
    let digest = value
        .get("contentDigest")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let produced = match (
        value.get("producerAttempt").and_then(|item| item.as_str()),
        value.get("outcome").and_then(|item| item.as_str()),
    ) {
        (Some(attempt), Some("succeeded")) if !attempt.is_empty() => Availability::Yes,
        (Some(_), Some(_)) => Availability::No,
        _ => Availability::NotMeasured,
    };
    let digest_verified = match value.get("digestVerified").and_then(|item| item.as_bool()) {
        Some(true) if !digest.is_empty() => Availability::Yes,
        Some(false) => Availability::No,
        _ => Availability::NotMeasured,
    };
    let accessible = match value.get("location").and_then(|item| item.as_str()) {
        Some(location) if std::path::Path::new(location).is_file() => Availability::Yes,
        Some(_) => Availability::No,
        None => Availability::NotMeasured,
    };
    let requirements_match = match_field(&value, "consumer", "expectedConsumer")
        .and_then(|consumer| {
            match_field(&value, "toolchain", "expectedToolchain")
                .map(|toolchain| consumer && toolchain)
        })
        .and_then(|so_far| {
            match_field(&value, "platform", "expectedPlatform").map(|platform| so_far && platform)
        })
        .and_then(|so_far| {
            match_field(&value, "source", "expectedSource").map(|source| so_far && source)
        })
        .map(|matched| {
            if matched {
                Availability::Yes
            } else {
                Availability::No
            }
        })
        .unwrap_or(Availability::NotMeasured);
    ArtifactAvailability {
        schema: "artifact-availability-v1".to_owned(),
        produced,
        digest_verified,
        accessible,
        requirements_match,
    }
}

fn match_field(value: &serde_json::Value, left: &str, right: &str) -> Option<bool> {
    let left = value.get(left)?.as_str()?;
    let right = value.get(right)?.as_str()?;
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some(left == right)
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
        let records = root.path().join(".nxr/artifacts/records");
        std::fs::create_dir_all(&records).unwrap();
        let location = root.path().join("mod.wasm");
        std::fs::write(&location, b"wasm").unwrap();
        let body = serde_json::json!({
            "contentDigest": "ab".repeat(32),
            "producerAttempt": "att",
            "outcome": "succeeded",
            "digestVerified": true,
            "location": location,
            "consumer": "worker-a",
            "expectedConsumer": "worker-a",
            "toolchain": "wasm-bindgen",
            "expectedToolchain": "wasm-bindgen",
            "platform": "wasm32",
            "expectedPlatform": "wasm32",
            "source": "tree",
            "expectedSource": "changed"
        });
        std::fs::write(records.join("stargate.json"), body.to_string()).unwrap();
        let availability = resolve_availability(root.path(), "stargate");
        assert_eq!(availability.produced, Availability::Yes);
        assert_eq!(availability.digest_verified, Availability::Yes);
        assert_eq!(availability.accessible, Availability::Yes);
        assert_eq!(availability.requirements_match, Availability::No);
        assert_eq!(reuse_decision(&availability), ReuseDecision::Execute);
        assert_eq!(
            resolve_availability(root.path(), "missing").produced,
            Availability::NotMeasured
        );
    }
}
