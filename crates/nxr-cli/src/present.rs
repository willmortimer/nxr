//! Plain-text presentation helpers for compact output and interaction prompts.

use std::collections::BTreeMap;
use std::io::{self, Write};

use nxr_task::TaskParameter;
use nxr_task::TaskParameterType;
use nxr_task::catalog::EffectClass;
use nxr_task::interaction::InputRequest;
use serde::Deserialize;
use serde_json::Value;

pub const PROGRESS_SCHEMA: &str = "nxr-progress-v1";

/// Subset of [`schemas/progress-v1.schema.json`] used for human lines.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressV1 {
    pub schema_version: String,
    pub node_id: String,
    pub stage_id: String,
    pub stage_label: String,
    #[serde(default)]
    pub completed: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
    pub unit: ProgressUnit,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ProgressUnit {
    Named(String),
    Custom { custom: String },
}

impl ProgressUnit {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Named(unit) => unit.as_str(),
            Self::Custom { custom } => custom.as_str(),
        }
    }
}

/// Parse a progress-v1 JSON object from phase detail or diagnostic text.
#[must_use]
pub fn parse_progress_json(text: &str) -> Option<ProgressV1> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let progress: ProgressV1 = serde_json::from_str(trimmed).ok()?;
    if progress.schema_version != PROGRESS_SCHEMA {
        return None;
    }
    Some(progress)
}

/// One human-readable progress line (no elapsed-time percentage).
#[must_use]
pub fn format_progress_line(progress: &ProgressV1) -> String {
    let count = match (progress.completed, progress.total) {
        (Some(completed), Some(total)) if progress.state != "indeterminate" => {
            format!("{completed}/{total}")
        }
        _ => "indeterminate".to_owned(),
    };
    let unit = progress
        .unit
        .as_str()
        .trim()
        .chars()
        .take(32)
        .collect::<String>();
    let unit = if unit.is_empty() || count == "indeterminate" {
        String::new()
    } else {
        format!(" {}", unit)
    };
    format!(
        "[{}] {}: {count}{unit} · {}",
        sanitize_one_line(&progress.node_id),
        sanitize_one_line(&progress.stage_label),
        sanitize_one_line(&progress.state)
    )
}

struct CausalRule {
    label: &'static str,
    /// Match these substrings in diagnostic or reason text.
    phrases: &'static [&'static str],
    /// When true, only [`phrases`] that equal the label are considered.
    label_phrase_only: bool,
}

const CAUSAL_RULES: &[CausalRule] = &[
    CausalRule {
        label: "submit failure",
        phrases: &["submit failure"],
        label_phrase_only: true,
    },
    CausalRule {
        label: "admission refused",
        phrases: &["admission refused", "submit refused", "admission denied"],
        label_phrase_only: false,
    },
    CausalRule {
        label: "queued too long",
        phrases: &["queued too long"],
        label_phrase_only: true,
    },
    CausalRule {
        label: "worker disappeared",
        phrases: &[
            "worker disappeared",
            "worker unavailable",
            "worker lost",
            "lease expired",
        ],
        label_phrase_only: false,
    },
    CausalRule {
        label: "execution failed",
        phrases: &["execution failed", "node failed"],
        label_phrase_only: false,
    },
    CausalRule {
        label: "artifact missing",
        phrases: &["artifact missing"],
        label_phrase_only: true,
    },
    CausalRule {
        label: "digest mismatch",
        phrases: &[
            "digest mismatch",
            "artifact digest mismatch",
            "sha256 mismatch",
        ],
        label_phrase_only: false,
    },
    CausalRule {
        label: "result uncertain",
        phrases: &[
            "result uncertain",
            "status\":\"uncertain",
            "launch acknowledgement unavailable",
        ],
        label_phrase_only: false,
    },
];

/// Distinct causal lines derived from reason or diagnostic text.
///
/// Does not collapse multiple categories into one summary line.
#[must_use]
pub fn causal_failure_lines(
    node: &str,
    reason: Option<&str>,
    diagnostic: Option<&str>,
) -> Vec<String> {
    let mut corpus = String::new();
    if let Some(reason) = reason.filter(|text| !text.is_empty()) {
        corpus.push_str(reason);
        corpus.push('\n');
    }
    if let Some(message) = diagnostic.filter(|text| !text.is_empty()) {
        corpus.push_str(message);
    }
    if corpus.is_empty() {
        return Vec::new();
    }
    let lower = corpus.to_ascii_lowercase();
    let mut lines = Vec::new();
    for rule in CAUSAL_RULES {
        let matched = if rule.label_phrase_only {
            lower.contains(rule.label)
        } else {
            rule.phrases.iter().any(|phrase| lower.contains(phrase))
        };
        if !matched {
            continue;
        }
        let detail = causal_detail(&corpus, rule.phrases);
        lines.push(format_causal_line(rule.label, node, detail));
    }
    lines
}

fn causal_detail(corpus: &str, phrases: &[&str]) -> String {
    let lower = corpus.to_ascii_lowercase();
    let Some(phrase) = phrases.iter().find(|phrase| lower.contains(*phrase)) else {
        return sanitize_one_line(corpus);
    };
    corpus
        .lines()
        .find(|line| line.to_ascii_lowercase().contains(phrase))
        .map(|line| sanitize_one_line(line.trim()))
        .unwrap_or_else(|| sanitize_one_line(corpus))
}

fn format_causal_line(label: &str, node: &str, detail: String) -> String {
    if detail.is_empty() {
        format!("[nxr] {label}: {}", sanitize_one_line(node))
    } else {
        format!("[nxr] {label}: {} — {}", sanitize_one_line(node), detail)
    }
}

/// Write one informative line (never clears the screen or moves the cursor).
pub fn write_informative_line(
    writer: &mut dyn Write,
    line: &str,
    _stream_is_tty: bool,
) -> io::Result<()> {
    writeln!(writer, "{line}")?;
    writer.flush()
}

#[must_use]
pub fn parameter_required(declaration: &TaskParameter) -> bool {
    declaration.default.is_none()
}

#[must_use]
pub fn is_secret_parameter(declaration: &TaskParameter) -> bool {
    matches!(
        declaration.param_type,
        TaskParameterType::SecretRef | TaskParameterType::AuthorizationRef
    )
}

/// Prompt headline: parameter name, required/optional, constraints or default, origin.
#[must_use]
pub fn format_parameter_prompt(request: &InputRequest) -> String {
    let required = parameter_required(&request.declaration);
    let requirement = if required { "required" } else { "optional" };
    let mut parts = vec![
        format!(
            "task {} · parameter {} ({requirement})",
            request.node_id, request.parameter_name
        ),
        parameter_metadata_line(&request.declaration),
    ];
    if let Some(origin) = answer_origin_line(request) {
        parts.push(origin);
    }
    if is_secret_parameter(&request.declaration) {
        parts.push("secret value will not be shown".to_owned());
    }
    nxr_core::sanitize::sanitize_terminal_text(&parts.join(" · "))
}

fn parameter_metadata_line(declaration: &TaskParameter) -> String {
    if is_secret_parameter(declaration) {
        return "type: secret".to_owned();
    }
    if let Some(line) = constraint_summary(declaration) {
        return line;
    }
    if let Some(default) = &declaration.default {
        return format!(
            "default: {}",
            sanitize_one_line(&value_preview(default, declaration))
        );
    }
    format!("type: {:?}", declaration.param_type)
        .replace("TaskParameterType::", "")
        .to_ascii_lowercase()
}

fn constraint_summary(declaration: &TaskParameter) -> Option<String> {
    let constraints = declaration.constraints.as_ref()?;
    let mut parts = Vec::new();
    if let Some(min) = constraints.min_length {
        parts.push(format!("min_length={min}"));
    }
    if let Some(max) = constraints.max_length {
        parts.push(format!("max_length={max}"));
    }
    if let Some(pattern) = &constraints.pattern {
        parts.push(format!("pattern={pattern}"));
    }
    if let Some(minimum) = &constraints.minimum {
        parts.push(format!("minimum={minimum}"));
    }
    if let Some(maximum) = &constraints.maximum {
        parts.push(format!("maximum={maximum}"));
    }
    if !constraints.required.is_empty() {
        parts.push(format!(
            "required properties: {}",
            constraints.required.join(",")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("constraints: {}", parts.join(", ")))
    }
}

fn answer_origin_line(request: &InputRequest) -> Option<String> {
    request
        .observation_digest
        .as_ref()
        .map(|digest| format!("observation {}", sanitize_one_line(digest)))
}

fn value_preview(value: &Value, declaration: &TaskParameter) -> String {
    if is_secret_parameter(declaration) {
        return "(hidden)".to_owned();
    }
    sanitize_one_line(&value.to_string())
}

#[must_use]
pub fn effect_requires_prelaunch_review(effect: EffectClass) -> bool {
    matches!(
        effect,
        EffectClass::Remote | EffectClass::Deploy | EffectClass::LocalMutation
    )
}

/// Non-secret answers only; omit secret parameters entirely.
#[must_use]
pub fn format_prelaunch_review_lines(
    operation: &str,
    effect: EffectClass,
    answers: &BTreeMap<String, BTreeMap<String, (TaskParameter, Value)>>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "[nxr] review before launch · operation {operation} · effect {effect}"
    )];
    let mut any = false;
    for (node, parameters) in answers {
        for (name, (declaration, value)) in parameters {
            if is_secret_parameter(declaration) {
                continue;
            }
            any = true;
            lines.push(format!(
                "  {node}.{name} = {}",
                sanitize_one_line(&value_preview(value, declaration))
            ));
        }
    }
    if !any {
        lines.push("  (no non-secret answers)".to_owned());
    }
    lines
}

fn sanitize_one_line(text: &str) -> String {
    nxr_core::sanitize::sanitize_terminal_text(text)
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t') {
                ' '
            } else {
                ch
            }
        })
        .take(512)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::ParameterConstraints;
    use serde_json::json;

    fn sample_progress(completed: Option<u64>, total: Option<u64>, state: &str) -> ProgressV1 {
        ProgressV1 {
            schema_version: PROGRESS_SCHEMA.to_owned(),
            node_id: "rust-tests".to_owned(),
            stage_id: "nextest".to_owned(),
            stage_label: "compiler and lint".to_owned(),
            completed,
            total,
            unit: ProgressUnit::Named("tests".to_owned()),
            state: state.to_owned(),
        }
    }

    #[test]
    fn progress_line_formats_counts_and_unit() {
        let line = format_progress_line(&sample_progress(Some(3), Some(10), "running"));
        assert!(line.contains("compiler and lint: 3/10 tests"));
        assert!(!line.contains('%'));
    }

    #[test]
    fn progress_line_indeterminate_without_total() {
        let line = format_progress_line(&sample_progress(Some(3), None, "running"));
        assert!(line.contains(": indeterminate"));
        assert!(!line.contains("3/"));
    }

    #[test]
    fn progress_line_indeterminate_when_state_indeterminate() {
        let line = format_progress_line(&sample_progress(Some(3), Some(10), "indeterminate"));
        assert!(line.contains(": indeterminate"));
    }

    #[test]
    fn distinct_causal_lines_stay_separate() {
        let lines = causal_failure_lines(
            "remote",
            Some("digest mismatch on manifest"),
            Some("admission denied at gate"),
        );
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|line| line.contains("admission refused")));
        assert!(lines.iter().any(|line| line.contains("digest mismatch")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("remote build failed"))
        );
    }

    #[test]
    fn label_only_category_requires_exact_phrase() {
        let absent = causal_failure_lines("n", Some("queue timeout"), None);
        assert!(absent.is_empty());
        let present = causal_failure_lines("n", Some("queued too long at broker"), None);
        assert_eq!(present.len(), 1);
        assert!(present[0].contains("queued too long"));
    }

    #[test]
    fn secret_parameters_never_appear_in_prompt_text() {
        let request = InputRequest {
            request_id: "r".to_owned(),
            node_id: "deploy".to_owned(),
            parameter_name: "credential".to_owned(),
            declaration: TaskParameter {
                param_type: TaskParameterType::SecretRef,
                default: Some(json!("super-secret-token")),
                values: None,
                constraints: None,
            },
            observation_digest: None,
            revision: 0,
        };
        let prompt = format_parameter_prompt(&request);
        assert!(!prompt.contains("super-secret"));
        assert!(prompt.contains("secret value will not be shown"));
    }

    #[test]
    fn prompt_includes_constraints_and_required_flag() {
        let request = InputRequest {
            request_id: "r".to_owned(),
            node_id: "ci".to_owned(),
            parameter_name: "reason".to_owned(),
            declaration: TaskParameter {
                param_type: TaskParameterType::String,
                default: None,
                values: None,
                constraints: Some(ParameterConstraints {
                    min_length: Some(3),
                    max_length: Some(40),
                    ..Default::default()
                }),
            },
            observation_digest: Some("sha256:abcd".to_owned()),
            revision: 0,
        };
        let prompt = format_parameter_prompt(&request);
        assert!(prompt.contains("required"));
        assert!(prompt.contains("min_length=3"));
        assert!(prompt.contains("observation sha256:abcd"));
    }
}
