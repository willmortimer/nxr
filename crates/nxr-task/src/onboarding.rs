//! Onboarding workflow selection.
//!
//! Provision and recovery requests are [`PlanRequest`] values. Conditions are
//! limited to `all`, `any`, `equals`, `in`, and `compare`. Nothing here evaluates
//! a shell or starts a process.
//!
//! Callers supply observed facts. These constructors leave `facts` empty, so a
//! disk observation is never invented. [`plan_after_answers`](crate::planning::plan_after_answers)
//! still returns a [`FrozenPlan`] whose [`FrozenPlan::execution_permit`] is
//! `None`. [`destructive_branch_needs_new_grant`] reports that a selected
//! branch id contains `format` or `mutate`. It does not add an authorization
//! id, a lease, or a permit. `source_revision` and `declared_task_digest` are
//! template tokens; replace them with the caller's source identity before
//! freezing a real plan.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::planning::{ConditionalBranch, FrozenPlan, PlanRequest};

const SOURCE_REVISION: &str = "onboarding-v1";

/// How a recorded onboarding phase may be resumed.
///
/// [`ResumeBoundary::Uncertain`] is not a retry. It does not continue the open
/// phase and it does not replay a successful result. A phase this module does
/// not recognize is uncertain for the same reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeBoundary {
    /// `succeeded`: the recorded result can be replayed.
    Replay,
    /// `uncertain` or `lost-disko-ack`: inspect the existing run.
    Uncertain,
    /// `running` or `admitted`: the same phase is still open.
    Continue,
}

/// Provision branches over `network`, `credentials`, and `management`.
///
/// Expected fact values are `network` = `ethernet` | `wifi` | `none`,
/// `credentials` = `present` | `absent`, and `management` = `standalone` |
/// `hub`. The request does not store those observations.
#[must_use]
pub fn provision_request() -> PlanRequest {
    request(
        "onboarding-provision",
        vec![
            provision_arm(
                "provision-ethernet-standalone",
                "ethernet",
                "present",
                "standalone",
            ),
            provision_arm("provision-ethernet-hub", "ethernet", "present", "hub"),
            provision_arm("provision-wifi-standalone", "wifi", "present", "standalone"),
            provision_arm("provision-wifi-hub", "wifi", "present", "hub"),
            provision_arm("offline-standalone", "none", "present", "standalone"),
            provision_arm("offline-hub", "none", "present", "hub"),
            all_of(
                "await-credentials",
                &[
                    equals("credentials", "absent"),
                    any_of(&[
                        equals("network", "ethernet"),
                        equals("network", "wifi"),
                        equals("network", "none"),
                    ]),
                    one_of("management", &["standalone", "hub"]),
                ],
            ),
        ],
    )
}

/// Recovery branches over `firmware`, `disk`, and `installed`.
///
/// Expected fact values are `firmware` = `uefi` | `other`, `disk` = `stable` |
/// `missing`, and `installed` = `absent` | `present`. `format-disk` is selected
/// only when firmware is `uefi`, disk is `stable`, and installed is `absent`.
/// The branch is representable here and still needs a new grant before any
/// destructive step. This function does not observe a disk.
#[must_use]
pub fn recovery_request() -> PlanRequest {
    request(
        "onboarding-recovery",
        vec![
            all_of(
                "format-disk",
                &[
                    equals("firmware", "uefi"),
                    equals("disk", "stable"),
                    equals("installed", "absent"),
                ],
            ),
            all_of(
                "recover-installed",
                &[
                    equals("firmware", "uefi"),
                    equals("disk", "stable"),
                    equals("installed", "present"),
                ],
            ),
            all_of(
                "disk-missing",
                &[
                    equals("firmware", "uefi"),
                    equals("disk", "missing"),
                    one_of("installed", &["absent", "present"]),
                ],
            ),
            all_of(
                "unsupported-firmware",
                &[
                    equals("firmware", "other"),
                    one_of("disk", &["stable", "missing"]),
                    one_of("installed", &["absent", "present"]),
                ],
            ),
        ],
    )
}

/// Whether a selected branch id contains `format` or `mutate`.
///
/// True means a separate grant is required. This does not insert an
/// authorization id and does not change [`FrozenPlan::execution_permit`].
#[must_use]
pub fn destructive_branch_needs_new_grant(plan: &FrozenPlan) -> bool {
    plan.selected_branches
        .iter()
        .any(|id| id.contains("format") || id.contains("mutate"))
}

/// Resume boundary for one recorded phase.
///
/// `uncertain` and `lost-disko-ack` are [`ResumeBoundary::Uncertain`].
/// `running` and `admitted` continue. `succeeded` replays. Any other phase is
/// uncertain. Uncertain is not a retry.
#[must_use]
pub fn resume_boundary(phase: &str) -> ResumeBoundary {
    match phase {
        "uncertain" | "lost-disko-ack" => ResumeBoundary::Uncertain,
        "running" | "admitted" => ResumeBoundary::Continue,
        "succeeded" => ResumeBoundary::Replay,
        _ => ResumeBoundary::Uncertain,
    }
}

/// Headless sessions record answers. A stdout TTY may prompt.
#[must_use]
pub const fn headless_interaction(stdout_is_tty: bool) -> &'static str {
    if stdout_is_tty { "prompt" } else { "record" }
}

fn request(declared_task_digest: &str, branches: Vec<ConditionalBranch>) -> PlanRequest {
    PlanRequest {
        source_revision: SOURCE_REVISION.to_owned(),
        dirty: false,
        declared_task_digest: declared_task_digest.to_owned(),
        ordinary_inputs: BTreeMap::new(),
        facts: BTreeMap::new(),
        branches,
        artifact_ids: BTreeSet::new(),
        authorization_ids: BTreeSet::new(),
    }
}

fn provision_arm(
    id: &str,
    network: &str,
    credentials: &str,
    management: &str,
) -> ConditionalBranch {
    all_of(
        id,
        &[
            equals("network", network),
            equals("credentials", credentials),
            equals("management", management),
        ],
    )
}

fn all_of(id: &str, parts: &[Value]) -> ConditionalBranch {
    ConditionalBranch {
        id: id.to_owned(),
        condition: json!({ "all": parts }),
    }
}

fn any_of(parts: &[Value]) -> Value {
    json!({ "any": parts })
}

fn equals(fact: &str, value: &str) -> Value {
    json!({ "equals": { "fact": fact, "value": value } })
}

fn one_of(fact: &str, values: &[&str]) -> Value {
    json!({ "in": { "fact": fact, "values": values } })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{
        ResumeBoundary, destructive_branch_needs_new_grant, headless_interaction,
        provision_request, recovery_request, resume_boundary,
    };
    use crate::planning::{
        DiscoverySource, FrozenPlan, NullDiscovery, PlanRequest, PlanningError, plan_after_answers,
    };

    fn ethernet_standalone() -> FrozenPlan {
        let discovery = NullDiscovery.discover().expect("null discovery");
        let mut request = provision_request();
        request
            .facts
            .insert("network".to_owned(), json!("ethernet"));
        request
            .facts
            .insert("credentials".to_owned(), json!("present"));
        request
            .facts
            .insert("management".to_owned(), json!("standalone"));
        plan_after_answers(&discovery, &request, &BTreeMap::new()).expect("frozen plan")
    }

    fn condition_text(request: &PlanRequest) -> String {
        request
            .branches
            .iter()
            .map(|branch| serde_json::to_string(&branch.condition).expect("condition"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ethernet_credentials_standalone_selects_a_non_destructive_branch() {
        let request = provision_request();
        assert!(request.facts.is_empty());
        let encoded = condition_text(&request);
        assert!(!encoded.contains("shell"));

        let plan = ethernet_standalone();
        assert!(
            plan.selected_branches
                .contains("provision-ethernet-standalone")
        );
        assert_eq!(plan.selected_branches.len(), 1);
        assert!(
            plan.selected_branches
                .iter()
                .all(|id| !id.contains("format") && !id.contains("mutate"))
        );
        assert!(!destructive_branch_needs_new_grant(&plan));
        assert_eq!(plan.execution_permit(), None);
    }

    #[test]
    fn format_branch_sets_destructive_branch_needs_new_grant() {
        let recovery = recovery_request();
        assert!(recovery.facts.is_empty());
        assert!(!recovery.facts.contains_key("disk"));
        assert!(
            recovery
                .branches
                .iter()
                .any(|branch| branch.id == "format-disk")
        );
        let encoded = condition_text(&recovery);
        assert!(!encoded.contains("shell"));
        assert!(encoded.contains("\"disk\""));

        let discovery = NullDiscovery.discover().expect("null discovery");
        let mut request = recovery_request();
        request.facts.insert("firmware".to_owned(), json!("uefi"));
        request.facts.insert("disk".to_owned(), json!("stable"));
        request
            .facts
            .insert("installed".to_owned(), json!("absent"));
        let plan = plan_after_answers(&discovery, &request, &BTreeMap::new()).expect("frozen");
        assert!(plan.selected_branches.contains("format-disk"));
        assert!(destructive_branch_needs_new_grant(&plan));
        assert!(plan.authorization_ids.is_empty());
        assert_eq!(plan.execution_permit(), None);

        let mut mutate = ethernet_standalone();
        mutate.selected_branches.insert("mutate-boot".to_owned());
        assert!(destructive_branch_needs_new_grant(&mutate));
        assert_eq!(mutate.execution_permit(), None);
    }

    #[test]
    fn recovery_does_not_invent_disk_facts() {
        let discovery = NullDiscovery.discover().expect("null discovery");
        let mut request = recovery_request();
        request.facts.insert("firmware".to_owned(), json!("uefi"));
        request
            .facts
            .insert("installed".to_owned(), json!("absent"));
        assert!(!request.facts.contains_key("disk"));
        assert_eq!(
            plan_after_answers(&discovery, &request, &BTreeMap::new()),
            Err(PlanningError::UnknownFact {
                name: "disk".to_owned(),
            })
        );
    }

    #[test]
    fn lost_disko_ack_is_uncertain_and_not_a_retry() {
        assert_eq!(resume_boundary("lost-disko-ack"), ResumeBoundary::Uncertain);
        assert_eq!(resume_boundary("uncertain"), ResumeBoundary::Uncertain);
        assert_eq!(resume_boundary("running"), ResumeBoundary::Continue);
        assert_eq!(resume_boundary("admitted"), ResumeBoundary::Continue);
        assert_eq!(resume_boundary("succeeded"), ResumeBoundary::Replay);
        assert_eq!(resume_boundary("publishing"), ResumeBoundary::Uncertain);
        let boundary = resume_boundary("lost-disko-ack");
        assert_ne!(boundary, ResumeBoundary::Replay);
        assert_ne!(boundary, ResumeBoundary::Continue);
    }

    #[test]
    fn headless_is_record() {
        assert_eq!(headless_interaction(false), "record");
        assert_eq!(headless_interaction(true), "prompt");
    }

    #[test]
    fn execution_permit_is_none_after_plan_after_answers() {
        let discovery = NullDiscovery.discover().expect("null discovery");
        let plan = ethernet_standalone();
        assert_eq!(plan.discovery_digest, discovery.digest);
        assert_eq!(plan.execution_permit(), None);
        assert!(plan.authorization_ids.is_empty());
        let encoded = serde_json::to_string(&plan).expect("serialize");
        assert!(!encoded.contains("ExecutionLease"));
        assert!(!encoded.contains("format-disk"));
    }
}
