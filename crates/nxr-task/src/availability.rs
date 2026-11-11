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
}
