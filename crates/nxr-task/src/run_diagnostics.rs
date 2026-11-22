//! Bounded parse of supervisor run logs into coarse failure categories.

use std::io::{self, BufRead};

use serde::{Deserialize, Serialize};

const MAX_DIAGNOSTICS: usize = 8;
const MAX_EXCERPT: usize = 240;
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_SCAN_BYTES: usize = 4 * 1024 * 1024;

/// Coarse diagnostic derived from supervisor log lines.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunDiagnostic {
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub excerpt: String,
    pub digest: Option<String>,
    pub object: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub reference: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub node_id: String,
    pub location: Option<SourceLocation>,
    pub evidence: Vec<EvidenceRef>,
    pub fingerprint: String,
}

impl RunDiagnostic {
    /// Stable normalized adapter; original log bytes remain in the journal.
    #[must_use]
    pub fn normalized(&self, node: &str) -> Diagnostic {
        use sha2::{Digest, Sha256};
        let location = self.file.as_ref().map(|file| SourceLocation {
            file: file.clone(),
            line: self.line,
        });
        let preimage = serde_json::json!([self.category, self.code, node, location, self.excerpt]);
        let fingerprint = format!(
            "sha256:{:x}",
            Sha256::digest(preimage.to_string().as_bytes())
        );
        let mut evidence = Vec::new();
        if let Some(object) = &self.object {
            evidence.push(EvidenceRef {
                kind: "artifact-declaration".into(),
                reference: object.clone(),
            });
        }
        Diagnostic {
            code: self.code.clone().unwrap_or_else(|| self.category.clone()),
            severity: self
                .severity
                .unwrap_or(if self.category == "diagnostic_artifact" {
                    Severity::Info
                } else {
                    Severity::Error
                }),
            message: self.excerpt.clone(),
            node_id: node.into(),
            location,
            evidence,
            fingerprint,
        }
    }
}

/// Parse a bounded slice of diagnostic signals from a supervisor log.
#[must_use]
pub fn parse_run_log(log: &str) -> Vec<RunDiagnostic> {
    parse_run_log_reader(io::Cursor::new(log.as_bytes())).unwrap_or_default()
}

/// Scan without collecting a log or allocating an unbounded physical line.
/// Oversized lines are discarded; inspect raw byte slices for their evidence.
pub fn parse_run_log_reader(mut reader: impl BufRead) -> io::Result<Vec<RunDiagnostic>> {
    let mut out = Vec::new();
    let mut fallback = None;
    let mut line = Vec::new();
    let mut oversized = false;
    let mut scanned = 0;
    while scanned < MAX_SCAN_BYTES && out.len() < MAX_DIAGNOSTICS {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let available = buffer.len().min(MAX_SCAN_BYTES - scanned);
        let consumed = buffer[..available]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available, |position| position + 1);
        let complete = buffer[consumed - 1] == b'\n';
        if !oversized && line.len() + consumed <= MAX_LINE_BYTES {
            line.extend_from_slice(&buffer[..consumed]);
        } else {
            line.clear();
            oversized = true;
        }
        reader.consume(consumed);
        scanned += consumed;
        if complete {
            if !oversized {
                process_diagnostic_line(&String::from_utf8_lossy(&line), &mut out, &mut fallback);
            }
            line.clear();
            oversized = false;
        }
    }
    if !line.is_empty() && !oversized && scanned < MAX_SCAN_BYTES && out.len() < MAX_DIAGNOSTICS {
        process_diagnostic_line(&String::from_utf8_lossy(&line), &mut out, &mut fallback);
    }
    if out.is_empty()
        && let Some(excerpt) = fallback
    {
        out.push(RunDiagnostic {
            code: None,
            severity: None,
            category: "unknown".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }
    Ok(out)
}

fn process_diagnostic_line(
    line: &str,
    out: &mut Vec<RunDiagnostic>,
    fallback: &mut Option<String>,
) {
    if line.trim().is_empty() || is_secret_line(line) {
        return;
    }
    if fallback.is_none() {
        *fallback = Some(truncate_excerpt(line));
    }
    if let Some(previous) = out.last_mut()
        && previous.category == "compile_error"
        && previous.file.is_none()
        && let Some((file, line_no)) = parse_rustc_arrow(line)
    {
        previous.file = Some(file);
        previous.line = Some(line_no);
    }
    if let Some(diagnostic) = classify_line(line)
        && !out.contains(&diagnostic)
    {
        out.push(diagnostic);
    }
}

fn truncate_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    trimmed.chars().take(MAX_EXCERPT).collect()
}

fn is_secret_line(line: &str) -> bool {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    trimmed.contains("KEY=")
        || trimmed.contains("TOKEN=")
        || lower.contains("sops")
        || lower.contains("password=")
        || lower.contains("secret=")
        || lower.contains("token=")
        || lower.contains("authorization:")
        || lower.contains("private key-----")
}

fn classify_line(line: &str) -> Option<RunDiagnostic> {
    // Structured adapters precede coarse text fallback. Adding another domain
    // format does not let child records become execution authority.
    const STRUCTURED: &[fn(&str) -> Option<RunDiagnostic>] = &[
        classify_diagnostic_artifact,
        classify_cargo_json,
        classify_nix_json,
        classify_test_result,
        classify_domain_error,
    ];
    for adapter in STRUCTURED {
        if let Some(diagnostic) = adapter(line) {
            return Some(diagnostic);
        }
    }

    let excerpt = truncate_excerpt(line);

    if line.contains("hub request failed at ") {
        return Some(diagnostic("nixplane_transport", line));
    }
    if line.contains("submit refused") || line.contains("admission denied") {
        return Some(diagnostic("admission_refusal", line));
    }

    if is_plain_rustc_error(line) {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "compile_error".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }

    if let Some((file, line_no)) = parse_panic_location(line) {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "test_assertion".into(),
            file: Some(file),
            line: Some(line_no),
            excerpt,
            digest: None,
            object: None,
        });
    }

    if is_test_assertion_text(line) {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "test_assertion".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }

    if is_missing_toolchain(line) {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "missing_toolchain".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }

    if line.contains("policy approval required") || line.contains("policy refusal") {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "policy_refusal".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }

    if line.contains("local budget exceeded") || line.contains("already has a running local build")
    {
        return Some(RunDiagnostic {
            code: None,
            severity: None,
            category: "resource_refusal".into(),
            file: None,
            line: None,
            excerpt,
            digest: None,
            object: None,
        });
    }

    None
}

fn classify_nix_json(line: &str) -> Option<RunDiagnostic> {
    let payload = line.trim().strip_prefix("@nix ")?;
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("action")?.as_str()? != "msg" || value.get("level")?.as_u64()? > 1 {
        return None;
    }
    let message = value.get("msg")?.as_str()?;
    Some(RunDiagnostic {
        code: None,
        severity: Some(if value.get("level")?.as_u64()? == 1 {
            Severity::Warning
        } else {
            Severity::Error
        }),
        category: "nix_error".into(),
        file: None,
        line: None,
        excerpt: truncate_excerpt(message),
        digest: None,
        object: None,
    })
}

fn classify_diagnostic_artifact(line: &str) -> Option<RunDiagnostic> {
    let reference = crate::artifact_ref::ArtifactRef::parse_declaration(line).ok()?;
    let digest = reference.digest.strip_prefix("sha256:")?;
    let crate::artifact_ref::ArtifactLocation::LocalCas { object } = &reference.location;
    Some(RunDiagnostic {
        code: None,
        severity: None,
        category: "diagnostic_artifact".into(),
        file: None,
        line: None,
        excerpt: truncate_excerpt(line),
        digest: Some(digest.to_owned()),
        object: Some(object.to_owned()),
    })
}

fn classify_cargo_json(line: &str) -> Option<RunDiagnostic> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }

    let message = value.get("message")?;
    let level = message.get("level")?.as_str()?;
    if !matches!(level, "error" | "warning") {
        return None;
    }

    let mut file = None;
    let mut line_no = None;
    if let Some(spans) = message.get("spans").and_then(|s| s.as_array())
        && let Some(first) = spans.first().and_then(|s| s.as_object())
    {
        if let Some(name) = first.get("file_name").and_then(|n| n.as_str()) {
            file = Some(name.to_owned());
        }
        if let Some(start) = first.get("line_start").and_then(|n| n.as_u64()) {
            line_no = Some(u32::try_from(start).unwrap_or(u32::MAX));
        }
    }

    Some(RunDiagnostic {
        code: message
            .pointer("/code/code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        severity: Some(if level == "warning" {
            Severity::Warning
        } else {
            Severity::Error
        }),
        category: if level == "warning" {
            "compile_warning"
        } else {
            "compile_error"
        }
        .into(),
        file,
        line: line_no,
        excerpt: truncate_excerpt(message.get("message")?.as_str()?),
        digest: None,
        object: None,
    })
}

fn diagnostic(category: &str, message: &str) -> RunDiagnostic {
    RunDiagnostic {
        category: category.into(),
        code: None,
        severity: None,
        file: None,
        line: None,
        excerpt: truncate_excerpt(message),
        digest: None,
        object: None,
    }
}

fn classify_test_result(line: &str) -> Option<RunDiagnostic> {
    let trimmed = line.trim();
    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        if value.get("type")?.as_str()? == "test" && value.get("event")?.as_str()? == "failed" {
            // Libtest JSON and Nextest's compatible JSON output. Do not copy
            // arbitrary captured test stdout into a compact diagnostic.
            return Some(diagnostic("test_failure", value.get("name")?.as_str()?));
        }
        return None;
    }
    if ["FAIL ", "TIMEOUT "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return Some(diagnostic("test_failure", trimmed));
    }
    // Recognize bounded JUnit failure elements in the queried log bytes. This
    // is not a report loader: no paths, entities, or external XML are opened.
    for tag in ["failure", "error"] {
        let marker = format!("<{tag}");
        let Some(start) = trimmed.find(&marker) else {
            continue;
        };
        let rest = &trimmed[start + marker.len()..];
        if rest.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            return Some(diagnostic("junit_failure", &trimmed[start..]));
        }
    }
    None
}

fn classify_domain_error(line: &str) -> Option<RunDiagnostic> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let class = value.get("error_class")?.as_str()?;
    // These are NxiError's published classes, not an assertion that an
    // installation grant, disk observation, or domain receipt is trustworthy.
    const NXI_CLASSES: &[&str] = &[
        "invalid-plan",
        "target-identity-mismatch",
        "missing-artifact",
        "untrusted-installer",
        "destructive-authorization-required",
        "approval-mismatch",
        "device-busy",
        "unsafe-existing-data",
        "interrupted-operation",
        "state-uncertain",
        "manual-recovery",
        "boot-verification-failed",
        "recovery-provider-failed",
        "hub-unavailable",
        "handoff-rejected",
    ];
    if !NXI_CLASSES.contains(&class) {
        return None;
    }
    let mut result = diagnostic("nxi_error", value.get("message")?.as_str()?);
    result.code = Some(format!("nxi.{class}"));
    Some(result)
}

fn is_plain_rustc_error(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("error:") || trimmed.starts_with("error[") || trimmed.contains(": error:")
}

fn parse_rustc_arrow(line: &str) -> Option<(String, u32)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("-->")?.trim();
    let (path, line_col) = rest.split_once(':')?;
    let line_str = line_col.split(':').next()?;
    let line_no = line_str.parse::<u32>().ok()?;
    Some((path.trim().to_owned(), line_no))
}

fn is_test_assertion_text(line: &str) -> bool {
    line.contains("assertion failed")
        || line.contains("assertion `left == right` failed")
        || line.contains("panicked at")
}

fn parse_panic_location(line: &str) -> Option<(String, u32)> {
    let marker = "panicked at ";
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let (path, line_col) = rest.split_once(':')?;
    let line_str = line_col.split(':').next()?;
    let line_no = line_str.parse::<u32>().ok()?;
    Some((path.trim().to_owned(), line_no))
}

fn is_missing_toolchain(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("command not found") {
        return true;
    }
    if lower.contains("no such file or directory")
        && (lower.contains("rustc") || lower.contains("cargo"))
    {
        return true;
    }
    lower.contains("toolchain") && lower.contains("not installed")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/run-diagnostics")
            .join(name);
        fs::read_to_string(path).expect("fixture readable")
    }

    fn first(log: &str) -> RunDiagnostic {
        let mut diags = parse_run_log(log);
        assert!(!diags.is_empty(), "expected at least one diagnostic");
        diags.remove(0)
    }

    #[test]
    fn compile_error_fixture() {
        let diag = first(&fixture("compile-error.log"));
        assert_eq!(diag.category, "compile_error");
        assert_eq!(diag.file.as_deref(), Some("src/main.rs"));
        assert_eq!(diag.line, Some(4));
    }

    #[test]
    fn cargo_json_error_fixture() {
        let diag = first(&fixture("cargo-json-error.log"));
        assert_eq!(diag.category, "compile_error");
        assert_eq!(diag.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(diag.line, Some(9));
        assert_eq!(diag.excerpt, "expected `;`, found `}`");
        assert_eq!(diag.normalized("compile").code, "E0308");
    }

    #[test]
    fn test_assertion_fixture() {
        let diags = parse_run_log(&fixture("test-assertion.log"));
        assert!(diags.iter().all(|d| d.category == "test_assertion"));
        let panic = &diags[0];
        assert_eq!(panic.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(panic.line, Some(12));
    }

    #[test]
    fn missing_toolchain_fixture() {
        let diag = first(&fixture("missing-toolchain.log"));
        assert_eq!(diag.category, "missing_toolchain");
        assert!(diag.file.is_none());
        assert!(diag.line.is_none());
    }

    #[test]
    fn policy_refusal_fixture() {
        let diag = first(&fixture("policy-refusal.log"));
        assert_eq!(diag.category, "policy_refusal");
    }

    #[test]
    fn resource_refusal_fixture() {
        let diag = first(&fixture("resource-refusal.log"));
        assert_eq!(diag.category, "resource_refusal");
    }

    #[test]
    fn unknown_fixture() {
        let diag = first(&fixture("unknown.log"));
        assert_eq!(diag.category, "unknown");
        assert!(diag.file.is_none());
        assert!(diag.line.is_none());
    }

    #[test]
    fn empty_log_returns_empty() {
        assert!(parse_run_log("").is_empty());
        assert!(parse_run_log("  \n  ").is_empty());
    }

    #[test]
    fn skips_secret_lines() {
        let log = "API_TOKEN=abc123\nhello world\n";
        let diags = parse_run_log(log);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].category, "unknown");
        assert!(!diags[0].excerpt.contains("TOKEN"));
    }

    #[test]
    fn diagnostic_artifact_carries_digest_and_object() {
        let digest = "a".repeat(64);
        let object = format!("objects/{digest}/recording.json");
        let line = format!(
            "lattice: diagnostic-artifact {{\"schema\":\"lattice.diagnostic.artifact.v1\",\"kind\":\"stargate-recording\",\"sha256\":\"{digest}\",\"object\":\"{object}\"}}"
        );
        let diag = first(&line);
        assert_eq!(diag.category, "diagnostic_artifact");
        assert_eq!(diag.digest.as_deref(), Some(digest.as_str()));
        assert_eq!(diag.object.as_deref(), Some(object.as_str()));
    }

    #[test]
    fn caps_at_eight() {
        let log = (0..20)
            .map(|i| format!("error: something bad #{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_run_log(&log).len(), MAX_DIAGNOSTICS);
    }

    #[test]
    fn unicode_excerpt_truncation_never_splits_a_codepoint() {
        let log = format!("error: {}", "🪐".repeat(300));
        let diagnostics = parse_run_log(&log);
        assert_eq!(diagnostics[0].excerpt.chars().count(), MAX_EXCERPT);
    }

    #[test]
    fn streaming_handles_short_buffers_and_unterminated_final_line() {
        let input = b"error: first\n --> src/lib.rs:7:1\nerror: final";
        let reader = io::BufReader::with_capacity(3, io::Cursor::new(input));
        let diagnostics = parse_run_log_reader(reader).expect("stream");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].file.as_deref(), Some("src/lib.rs"));
        assert_eq!(diagnostics[0].line, Some(7));
        assert_eq!(diagnostics[1].excerpt, "error: final");
    }

    #[test]
    fn oversized_physical_lines_are_discarded_without_hiding_later_errors() {
        let log = format!("{}\nerror: actual failure", "x".repeat(MAX_LINE_BYTES * 3));
        let diagnostics = parse_run_log(&log);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].excerpt, "error: actual failure");
    }

    #[test]
    fn repeated_identical_diagnostics_do_not_consume_the_limit() {
        let log = format!("{}error: independent", "error: repeated\n".repeat(30));
        let diagnostics = parse_run_log(&log);
        assert_eq!(diagnostics.len(), 2);
        let first = diagnostics[0].normalized("compile");
        assert_eq!(
            first.fingerprint,
            diagnostics[0].normalized("compile").fingerprint
        );
        assert_ne!(
            first.fingerprint,
            diagnostics[0].normalized("other-node").fingerprint
        );
    }

    #[test]
    fn nix_structured_failure_uses_its_domain_category() {
        let diagnostics =
            parse_run_log("@nix {\"action\":\"msg\",\"level\":0,\"msg\":\"builder failed\"}\n");
        assert_eq!(diagnostics[0].category, "nix_error");
        assert_eq!(diagnostics[0].excerpt, "builder failed");
    }

    #[test]
    fn cargo_warning_preserves_severity_and_primary_message() {
        let diagnostic = first(
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused import","code":{"code":"unused_imports"},"spans":[]}}"#,
        );
        let normalized = diagnostic.normalized("compile");
        assert_eq!(normalized.severity, Severity::Warning);
        assert_eq!(normalized.message, "unused import");
        assert_eq!(normalized.code, "unused_imports");
    }

    #[test]
    fn test_adapters_classify_failures_without_promoting_test_output_to_authority() {
        assert_eq!(first("FAIL [0.1s] package::test").category, "test_failure");
        assert_eq!(
            first("TIMEOUT [60s] package::test").category,
            "test_failure"
        );
        let diagnostic = first(
            r#"{"type":"test","event":"failed","name":"crate::failure","stdout":"run_completed"}"#,
        );
        assert_eq!(diagnostic.excerpt, "crate::failure");
        assert_eq!(diagnostic.category, "test_failure");
        assert_eq!(
            first(r#"<testcase name="failure"><failure message="assertion"/></testcase>"#).category,
            "junit_failure"
        );
        assert_eq!(first("<failures>0</failures>").category, "unknown");
        assert_eq!(first("XFAIL [0.1s] expected_failure").category, "unknown");
    }

    #[test]
    fn nxi_error_classes_preserve_domain_code_and_bounded_message() {
        let diagnostic = first(r#"{"error_class":"destructive-authorization-required","message":"destructive authorization required: no signed grant"}"#).normalized("install");
        assert_eq!(diagnostic.code, "nxi.destructive-authorization-required");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(
            first(r#"{"error_class":"caller-invented","message":"allowed"}"#).category,
            "unknown"
        );
    }

    #[test]
    fn remote_refusals_and_nix_warnings_keep_their_meaning() {
        assert_eq!(
            first("error: hub request failed at https://hub/v1: connection refused").category,
            "nixplane_transport"
        );
        assert_eq!(first("error: submit refused").category, "admission_refusal");
        assert_eq!(
            first(r#"@nix {"action":"msg","level":1,"msg":"evaluation warning"}"#)
                .normalized("build")
                .severity,
            Severity::Warning
        );
    }
}
