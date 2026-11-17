//! Qualification reuse decisions for prior passing records.
//!
//! Callers pass in-memory [`QualificationRecord`] values. Reuse is allowed only
//! for a previous passing qualification with tests that actually ran and
//! coverage that is known and complete. Fail closed.

/// A qualification attempt record used for reuse decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QualificationRecord {
    pub status: String,
    pub coverage: String,
    pub source_digest: String,
    pub test_digest: String,
    pub toolchain: String,
    pub parameters: String,
    pub tests_ran: bool,
}

/// Outcome of comparing a previous qualification against the current attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReuseDecision {
    Reuse,
    Invalidate { reason: String },
    Insufficient { reason: String },
    Refuse { reason: String },
}

/// Decide whether a previous passing qualification may be reused for `current`.
///
/// Evaluation order is fail-closed: missing, non-passing, incomplete coverage,
/// digest/toolchain/parameter drift, then current non-qualification, else reuse.
pub fn reuse_passing_qualification(
    previous: Option<&QualificationRecord>,
    current: &QualificationRecord,
) -> ReuseDecision {
    let Some(previous) = previous else {
        return ReuseDecision::Insufficient {
            reason: "no passing qualification".to_owned(),
        };
    };

    if previous.status != "passed" || !previous.tests_ran {
        return ReuseDecision::Refuse {
            reason: "not a passing qualification".to_owned(),
        };
    }

    if previous.coverage != "complete" {
        return ReuseDecision::Insufficient {
            reason: "unknown coverage".to_owned(),
        };
    }

    if previous.source_digest != current.source_digest {
        return ReuseDecision::Invalidate {
            reason: "source changed".to_owned(),
        };
    }

    if previous.test_digest != current.test_digest {
        return ReuseDecision::Invalidate {
            reason: "tests changed".to_owned(),
        };
    }

    if previous.toolchain != current.toolchain {
        return ReuseDecision::Invalidate {
            reason: "toolchain changed".to_owned(),
        };
    }

    if previous.parameters != current.parameters {
        return ReuseDecision::Invalidate {
            reason: "parameters changed".to_owned(),
        };
    }

    if current.status != "passed" || !current.tests_ran || current.coverage != "complete" {
        return ReuseDecision::Refuse {
            reason: "not a passing qualification".to_owned(),
        };
    }

    ReuseDecision::Reuse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passed_complete() -> QualificationRecord {
        QualificationRecord {
            status: "passed".to_owned(),
            coverage: "complete".to_owned(),
            source_digest: "source-a".to_owned(),
            test_digest: "tests-a".to_owned(),
            toolchain: "toolchain-a".to_owned(),
            parameters: "params-a".to_owned(),
            tests_ran: true,
        }
    }

    #[test]
    fn matching_complete_passed_records_reuse() {
        let previous = passed_complete();
        let current = passed_complete();
        assert_eq!(
            reuse_passing_qualification(Some(&previous), &current),
            ReuseDecision::Reuse
        );
    }

    #[test]
    fn missing_previous_is_insufficient() {
        let current = passed_complete();
        let decision = reuse_passing_qualification(None, &current);
        assert!(matches!(
            decision,
            ReuseDecision::Insufficient { reason } if reason.contains("no passing qualification")
        ));
    }

    #[test]
    fn failed_previous_is_refused() {
        let mut previous = passed_complete();
        previous.status = "failed".to_owned();
        let current = passed_complete();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Refuse { reason } if reason.contains("not a passing qualification")
        ));
    }

    #[test]
    fn skipped_previous_is_refused() {
        let mut previous = passed_complete();
        previous.status = "skipped".to_owned();
        let current = passed_complete();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Refuse { reason } if reason.contains("not a passing qualification")
        ));
    }

    #[test]
    fn tests_ran_false_is_refused() {
        let mut previous = passed_complete();
        previous.tests_ran = false;
        let current = passed_complete();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Refuse { reason } if reason.contains("not a passing qualification")
        ));
    }

    #[test]
    fn unknown_coverage_is_insufficient() {
        let mut previous = passed_complete();
        previous.coverage = "unknown".to_owned();
        let current = passed_complete();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Insufficient { reason } if reason.contains("unknown coverage")
        ));
    }

    #[test]
    fn partial_coverage_is_insufficient() {
        let mut previous = passed_complete();
        previous.coverage = "partial".to_owned();
        let current = passed_complete();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Insufficient { reason } if reason.contains("unknown coverage")
        ));
    }

    #[test]
    fn source_change_invalidates() {
        let previous = passed_complete();
        let mut current = passed_complete();
        current.source_digest = "source-b".to_owned();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Invalidate { reason } if reason.contains("source changed")
        ));
    }

    #[test]
    fn toolchain_change_invalidates() {
        let previous = passed_complete();
        let mut current = passed_complete();
        current.toolchain = "toolchain-b".to_owned();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Invalidate { reason } if reason.contains("toolchain changed")
        ));
    }

    #[test]
    fn parameter_change_invalidates() {
        let previous = passed_complete();
        let mut current = passed_complete();
        current.parameters = "params-b".to_owned();
        let decision = reuse_passing_qualification(Some(&previous), &current);
        assert!(matches!(
            decision,
            ReuseDecision::Invalidate { reason } if reason.contains("parameters changed")
        ));
    }
}
