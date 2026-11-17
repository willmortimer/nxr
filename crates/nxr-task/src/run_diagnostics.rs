//! Bounded parse of supervisor run logs into coarse failure categories.

const MAX_DIAGNOSTICS: usize = 8;
const MAX_EXCERPT: usize = 240;

/// Coarse diagnostic derived from supervisor log lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunDiagnostic {
    pub category: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub excerpt: String,
}

/// Parse a bounded slice of diagnostic signals from a supervisor log.
#[must_use]
pub fn parse_run_log(log: &str) -> Vec<RunDiagnostic> {
    if log.trim().is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = log.lines().collect();
    let mut out = Vec::new();
    let mut index = 0;

    while index < lines.len() && out.len() < MAX_DIAGNOSTICS {
        let line = lines[index];
        if is_secret_line(line) {
            index += 1;
            continue;
        }

        let Some(mut diagnostic) = classify_line(line) else {
            index += 1;
            continue;
        };

        if diagnostic.category == "compile_error"
            && diagnostic.file.is_none()
            && index + 1 < lines.len()
        {
            let next = lines[index + 1];
            if !is_secret_line(next)
                && let Some((file, line_no)) = parse_rustc_arrow(next)
            {
                diagnostic.file = Some(file);
                diagnostic.line = Some(line_no);
            }
        }

        out.push(diagnostic);
        index += 1;
    }

    if out.is_empty()
        && let Some(line) = first_meaningful_line(log)
    {
        return vec![RunDiagnostic {
            category: "unknown".into(),
            file: None,
            line: None,
            excerpt: truncate_excerpt(line),
        }];
    }

    out
}

fn first_meaningful_line(log: &str) -> Option<&str> {
    log.lines()
        .find(|line| !line.trim().is_empty() && !is_secret_line(line))
}

fn truncate_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= MAX_EXCERPT {
        return trimmed.to_owned();
    }
    trimmed[..MAX_EXCERPT].to_owned()
}

fn is_secret_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains("KEY=")
        || trimmed.contains("TOKEN=")
        || trimmed.to_ascii_lowercase().contains("sops")
}

fn classify_line(line: &str) -> Option<RunDiagnostic> {
    if let Some(diagnostic) = classify_cargo_json(line) {
        return Some(diagnostic);
    }

    let excerpt = truncate_excerpt(line);

    if is_plain_rustc_error(line) {
        return Some(RunDiagnostic {
            category: "compile_error".into(),
            file: None,
            line: None,
            excerpt,
        });
    }

    if let Some((file, line_no)) = parse_panic_location(line) {
        return Some(RunDiagnostic {
            category: "test_assertion".into(),
            file: Some(file),
            line: Some(line_no),
            excerpt,
        });
    }

    if is_test_assertion_text(line) {
        return Some(RunDiagnostic {
            category: "test_assertion".into(),
            file: None,
            line: None,
            excerpt,
        });
    }

    if is_missing_toolchain(line) {
        return Some(RunDiagnostic {
            category: "missing_toolchain".into(),
            file: None,
            line: None,
            excerpt,
        });
    }

    if line.contains("policy approval required") || line.contains("policy refusal") {
        return Some(RunDiagnostic {
            category: "policy_refusal".into(),
            file: None,
            line: None,
            excerpt,
        });
    }

    if line.contains("local budget exceeded") || line.contains("already has a running local build")
    {
        return Some(RunDiagnostic {
            category: "resource_refusal".into(),
            file: None,
            line: None,
            excerpt,
        });
    }

    None
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
    if message.get("level")?.as_str()? != "error" {
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
        category: "compile_error".into(),
        file,
        line: line_no,
        excerpt: truncate_excerpt(line),
    })
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
    fn caps_at_eight() {
        let log = (0..20)
            .map(|i| format!("error: something bad #{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_run_log(&log).len(), MAX_DIAGNOSTICS);
    }
}
