//! Attach a second caller to equivalent verified work.
//!
//! An idempotency key replays one caller. Coalescing attaches another caller
//! to an active run only when every execution-identity field matches and that
//! run is verified. Parameters are already canonical and are compared as given.
//! Resource overlap is exact string equality. Distinct cohorts are already
//! distinct resource strings.

/// Fields that decide whether two runs are the same verified execution.
///
/// Every field is required. Comparison is exact and does not reorder
/// `parameters`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionIdentity {
    pub operation_id: String,
    pub parameters: String,
    pub source_digest: String,
    pub toolchain: String,
    pub platform: String,
    pub env_digest: String,
}

/// One in-flight run that a new caller might join or collide with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveRun {
    pub run_id: String,
    pub identity: ExecutionIdentity,
    pub verified: bool,
    pub resources: Vec<String>,
    pub observers: u32,
}

/// Whether the candidate joins an active run, collides, or starts new work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoalesceDecision {
    /// Join this verified run. The first matching active run wins.
    Attach { run_id: String },
    /// An active run holds a resource string the candidate also needs.
    Conflict { reason: String },
    /// No verified equivalent run and no shared resource string.
    Start,
}

/// Decide whether a verified candidate attaches, conflicts, or starts.
///
/// Attach is considered before resource overlap. An unverified active run does
/// not accept an attach, and it still conflicts when a resource string matches.
///
/// # Errors
///
/// Returns an error containing `identity is unverified` when
/// `candidate_verified` is false or any identity field is empty. That candidate
/// does not attach.
pub fn coalesce_decision(
    candidate: &ExecutionIdentity,
    candidate_verified: bool,
    candidate_resources: &[String],
    active: &[ActiveRun],
) -> Result<CoalesceDecision, String> {
    if !candidate_verified || !identity_fields_present(candidate) {
        return Err("identity is unverified".to_owned());
    }

    if let Some(run) = active
        .iter()
        .find(|run| run.verified && run.identity == *candidate)
    {
        return Ok(CoalesceDecision::Attach {
            run_id: run.run_id.clone(),
        });
    }

    if active
        .iter()
        .any(|run| shares_resource(&run.resources, candidate_resources))
    {
        return Ok(CoalesceDecision::Conflict {
            reason: "resource conflict".to_owned(),
        });
    }

    Ok(CoalesceDecision::Start)
}

/// Cancel policy for one observer leaving a coalesced run.
///
/// More than one observer detaches. The last observer terminates the run.
///
/// # Errors
///
/// Returns an error containing `no observer` when `observers` is zero.
pub fn observer_cancel(observers: u32) -> Result<&'static str, String> {
    match observers {
        0 => Err("no observer".to_owned()),
        1 => Ok("terminate"),
        _ => Ok("detach"),
    }
}

fn identity_fields_present(identity: &ExecutionIdentity) -> bool {
    [
        identity.operation_id.as_str(),
        identity.parameters.as_str(),
        identity.source_digest.as_str(),
        identity.toolchain.as_str(),
        identity.platform.as_str(),
        identity.env_digest.as_str(),
    ]
    .iter()
    .all(|field| !field.is_empty())
}

fn shares_resource(active_resources: &[String], candidate_resources: &[String]) -> bool {
    active_resources
        .iter()
        .any(|held| candidate_resources.contains(held))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ExecutionIdentity {
        ExecutionIdentity {
            operation_id: "check".to_owned(),
            parameters: "{\"package\":\"nxr-task\"}".to_owned(),
            source_digest: "src-111".to_owned(),
            toolchain: "nixpkgs-22".to_owned(),
            platform: "aarch64-darwin".to_owned(),
            env_digest: "env-333".to_owned(),
        }
    }

    fn resources(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn active_run(
        run_id: &str,
        identity: ExecutionIdentity,
        verified: bool,
        resource_names: &[&str],
        observers: u32,
    ) -> ActiveRun {
        ActiveRun {
            run_id: run_id.to_owned(),
            identity,
            verified,
            resources: resources(resource_names),
            observers,
        }
    }

    #[test]
    fn identical_verified_identities_attach() {
        let candidate = identity();
        let mut other = identity();
        other.env_digest = "env-other".to_owned();
        let active = [
            active_run("run-other", other, true, &["shared"], 1),
            active_run("run-unverified", candidate.clone(), false, &["shared"], 1),
            active_run("run-first", candidate.clone(), true, &["elsewhere"], 0),
            active_run("run-second", candidate.clone(), true, &["shared"], 4),
        ];

        let decision = coalesce_decision(&candidate, true, &resources(&["shared"]), &active)
            .expect("verified identity");

        assert_eq!(
            decision,
            CoalesceDecision::Attach {
                run_id: "run-first".to_owned(),
            }
        );
    }

    #[test]
    fn unverified_candidate_errors() {
        let candidate = identity();
        let active = [active_run("run-live", candidate.clone(), true, &["cpu"], 2)];
        let err = coalesce_decision(&candidate, false, &resources(&["cpu"]), &active)
            .expect_err("unverified candidate must not attach");
        assert!(err.contains("identity is unverified"), "{err}");

        let blanks: [fn(&mut ExecutionIdentity); 6] = [
            |identity| identity.operation_id.clear(),
            |identity| identity.parameters.clear(),
            |identity| identity.source_digest.clear(),
            |identity| identity.toolchain.clear(),
            |identity| identity.platform.clear(),
            |identity| identity.env_digest.clear(),
        ];
        for clear in blanks {
            let mut broken = identity();
            clear(&mut broken);
            let err = coalesce_decision(&broken, true, &[], &active)
                .expect_err("empty identity field must not attach");
            assert!(err.contains("identity is unverified"), "{err}");
        }
    }

    #[test]
    fn same_resource_different_identity_conflicts() {
        let candidate = identity();
        let mut other = identity();
        other.toolchain = "other-toolchain".to_owned();
        let resources = resources(&["cohort-a", "lock"]);

        let verified_other = [active_run(
            "run-other",
            other,
            true,
            &["lock", "cohort-a"],
            1,
        )];
        let decision = coalesce_decision(&candidate, true, &resources, &verified_other)
            .expect("different identity");
        match decision {
            CoalesceDecision::Conflict { reason } => {
                assert!(reason.contains("resource conflict"), "{reason}");
            }
            CoalesceDecision::Attach { .. } | CoalesceDecision::Start => {
                panic!("shared resource must conflict, got {decision:?}");
            }
        }

        let unverified_same = [active_run(
            "run-same",
            candidate.clone(),
            false,
            &["lock"],
            1,
        )];
        let decision = coalesce_decision(&candidate, true, &resources, &unverified_same)
            .expect("unverified active run");
        match decision {
            CoalesceDecision::Conflict { reason } => {
                assert!(reason.contains("resource conflict"), "{reason}");
            }
            CoalesceDecision::Attach { .. } | CoalesceDecision::Start => {
                panic!("unverified active run must still conflict, got {decision:?}");
            }
        }
    }

    #[test]
    fn disjoint_resources_start() {
        let candidate = identity();
        let mut other = identity();
        other.platform = "x86_64-linux".to_owned();
        let active = [active_run(
            "run-other",
            other,
            true,
            &["cohort-a", "net"],
            1,
        )];

        let decision =
            coalesce_decision(&candidate, true, &resources(&["cohort-b", "disk"]), &active)
                .expect("disjoint resources");
        assert_eq!(decision, CoalesceDecision::Start);

        let decision = coalesce_decision(&candidate, true, &resources(&["cohort-b"]), &[])
            .expect("no active runs");
        assert_eq!(decision, CoalesceDecision::Start);

        let mut reordered = identity();
        reordered.parameters = "b,a".to_owned();
        let mut exact = identity();
        exact.parameters = "a,b".to_owned();
        let active = [active_run("run-params", reordered, true, &["cohort-a"], 1)];
        let decision = coalesce_decision(&exact, true, &resources(&["cohort-b"]), &active)
            .expect("canonical parameters stay unsorted");
        assert_eq!(decision, CoalesceDecision::Start);

        let unverified_same = [active_run(
            "run-unverified",
            candidate.clone(),
            false,
            &["cohort-a"],
            1,
        )];
        let decision = coalesce_decision(
            &candidate,
            true,
            &resources(&["cohort-b"]),
            &unverified_same,
        )
        .expect("unverified identity is not an attach");
        assert_eq!(decision, CoalesceDecision::Start);
    }

    #[test]
    fn two_observers_detach_and_the_last_observer_terminates() {
        let detach = observer_cancel(2).expect("two observers");
        assert_eq!(detach, "detach");
        assert!(!detach.contains("terminate"));
        assert_eq!(
            observer_cancel(3).expect("more than one observer"),
            "detach"
        );

        assert_eq!(observer_cancel(1).expect("last observer"), "terminate");

        let err = observer_cancel(0).expect_err("zero observers");
        assert!(err.contains("no observer"), "{err}");
    }
}
