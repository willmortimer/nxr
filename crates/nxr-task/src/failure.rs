//! Rule-based failure summary derived at terminal time.
//!
//! Categories come only from the phase and the evidence text already on the
//! event stream. When those do not support a category, the summary says
//! `unknown` rather than guessing.

use serde::Serialize;

/// `execution-failure-summary-v1`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FailureSummary {
    /// Schema name.
    pub schema: &'static str,
    /// Last observed phase.
    pub phase: String,
    /// `compilation`, `missing_tool`, `source_permission`, `worker_loss`,
    /// `transfer_corruption`, or `unknown`.
    pub category: &'static str,
    /// Where the failure was attributed.
    pub origin: &'static str,
    /// Whether the same attempt can be retried.
    pub retryable: bool,
    /// One sentence a person or agent can read.
    pub summary: String,
    /// Short evidence fragments. Never a secret value.
    pub evidence: Vec<String>,
    /// One rule-based next step.
    pub next_action: &'static str,
}

/// Build a summary from the last phase and already-collected evidence text.
#[must_use]
pub fn failure_summary(phase: &str, evidence_text: &str) -> FailureSummary {
    let text = evidence_text.to_ascii_lowercase();
    let (category, origin, retryable, summary, next_action) = classify(phase, &text);
    let evidence = evidence_refs(evidence_text);
    FailureSummary {
        schema: "execution-failure-summary-v1",
        phase: phase.to_owned(),
        category,
        origin,
        retryable,
        summary,
        evidence,
        next_action,
    }
}

fn classify(phase: &str, text: &str) -> (&'static str, &'static str, bool, String, &'static str) {
    if contains_any(text, &["class=transfer_corruption", "sha256 mismatch"]) {
        return (
            "transfer_corruption",
            "unknown",
            true,
            format!("The object digest did not match during {phase}."),
            "Discard the partial object and resume the granted range transfer.",
        );
    }
    if text.contains("class=source_permission") {
        return (
            "source_permission",
            "unknown",
            false,
            format!("Source access was refused during {phase}."),
            "Inspect the source grant. A status code alone does not prove it can be refreshed.",
        );
    }
    if contains_any(
        text,
        &[
            "command not found",
            "no such file",
            "missing tool",
            "not found in path",
        ],
    ) {
        return (
            "missing_tool",
            "worker",
            true,
            format!("A required tool was missing during {phase}."),
            "Install the missing tool on the worker image and retry the same attempt.",
        );
    }
    if contains_any(
        text,
        &[
            "worker lost",
            "lease expired",
            "worker unavailable",
            "connection reset",
        ],
    ) {
        return (
            "worker_loss",
            "worker",
            true,
            format!("The worker disappeared during {phase}."),
            "Reattach to the same attempt. Do not submit a second one.",
        );
    }
    if contains_any(
        text,
        &["error[e", "compilation", "cannot find", "linker", "rustc"],
    ) {
        return (
            "compilation",
            "producer",
            false,
            format!("The producer failed to compile during {phase}."),
            "Open the compiler diagnostic in the node log and fix the reported error.",
        );
    }
    (
        "unknown",
        "unknown",
        false,
        format!("The events do not support a category for phase {phase}."),
        "Inspect the phase and evidence. The events do not support a category.",
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn evidence_refs(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .map(redact_excerpt)
        .collect()
}

fn redact_excerpt(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if lower.contains("ghp_")
        || lower.contains("github_pat_")
        || lower.contains("bearer ")
        || lower.contains("token=")
        || lower.contains("://")
    {
        return "[redacted]".to_owned();
    }
    let mut excerpt = line.to_owned();
    if excerpt.len() > 120 {
        excerpt.truncate(120);
    }
    excerpt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_when_the_events_do_not_support_a_category() {
        let summary = failure_summary("running", "still going");
        assert_eq!(summary.schema, "execution-failure-summary-v1");
        assert_eq!(summary.category, "unknown");
        assert!(!summary.retryable);
        assert!(summary.summary.contains("unknown") || summary.summary.contains("do not"));
    }

    #[test]
    fn categories_follow_the_evidence() {
        assert_eq!(
            failure_summary("publishing", "sha256 mismatch").category,
            "transfer_corruption"
        );
        assert_eq!(
            failure_summary("preparing_source", "403 from the archive").category,
            "unknown"
        );
        assert!(!failure_summary("preparing_source", "403 from the archive").retryable);
        assert_eq!(
            failure_summary("preparing_source", "class=source_permission").category,
            "source_permission"
        );
        assert_eq!(
            failure_summary("preparing_source", "token=ghp_example").evidence,
            vec!["[redacted]".to_owned()]
        );
        assert_eq!(
            failure_summary("running", "command not found: attic").category,
            "missing_tool"
        );
        assert_eq!(
            failure_summary("assigned", "lease expired").category,
            "worker_loss"
        );
        assert_eq!(
            failure_summary("running", "error[E0425]: cannot find").category,
            "compilation"
        );
    }
}
