//! JUnit XML report writer derived from task run events.

use std::path::Path;

use nxr_task::NodeOutcome;

use super::{NodeReport, ReportWriteError, write_text};

/// Write a JUnit XML report for `nodes`.
///
/// When `nodes` is empty, emits a valid minimal document with zero tests.
///
/// # Errors
///
/// Returns [`ReportWriteError`] when the file cannot be written.
pub fn write_junit_report(
    path: &Path,
    nodes: &[NodeReport],
    run_duration_ms: Option<u64>,
) -> Result<(), ReportWriteError> {
    let mut failures = 0usize;
    let mut errors = 0usize;
    let mut skipped = 0usize;

    let mut testcase_lines = String::new();
    for node in nodes {
        let (outcome, failure_xml, error_xml, skip_xml) = junit_case_parts(node);
        match outcome {
            JunitOutcome::Passed => {}
            JunitOutcome::Failed => failures += 1,
            JunitOutcome::Error => errors += 1,
            JunitOutcome::Skipped => skipped += 1,
        }
        let seconds = node
            .duration_ms
            .map(|ms| format!("{:.3}", ms as f64 / 1000.0))
            .unwrap_or_else(|| "0".to_owned());
        testcase_lines.push_str(&format!(
            "    <testcase name=\"{}\" classname=\"nxr\" time=\"{seconds}\">{failure_xml}{error_xml}{skip_xml}    </testcase>\n",
            xml_escape(&node.name),
        ));
    }

    let suite_seconds = run_duration_ms
        .map(|ms| format!("{:.3}", ms as f64 / 1000.0))
        .unwrap_or_else(|| "0".to_owned());

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<testsuites>\n\
  <testsuite name=\"nxr\" tests=\"{}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{suite_seconds}\">\n\
{testcase_lines}\
  </testsuite>\n\
</testsuites>\n",
        nodes.len(),
    );
    write_text(path, &xml)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JunitOutcome {
    Passed,
    Failed,
    Error,
    Skipped,
}

fn junit_case_parts(node: &NodeReport) -> (JunitOutcome, String, String, String) {
    let status = node.status;
    let code = node.code.unwrap_or(0);

    if matches!(status, Some(NodeOutcome::Skipped)) {
        return (
            JunitOutcome::Skipped,
            String::new(),
            String::new(),
            "      <skipped/>\n".to_owned(),
        );
    }

    if matches!(status, Some(NodeOutcome::Cancelled)) {
        let message = xml_escape(node.reason.as_deref().unwrap_or("cancelled"));
        return (
            JunitOutcome::Error,
            String::new(),
            format!("      <error message=\"{message}\"/>\n"),
            String::new(),
        );
    }

    if matches!(status, Some(NodeOutcome::TimedOut)) {
        let message = xml_escape("timed out");
        return (
            JunitOutcome::Failed,
            format!("      <failure message=\"{message}\"/>\n"),
            String::new(),
            String::new(),
        );
    }

    if code == 0 {
        return (
            JunitOutcome::Passed,
            String::new(),
            String::new(),
            String::new(),
        );
    }

    let message = xml_escape(
        node.reason
            .as_deref()
            .unwrap_or("task exited with non-zero status"),
    );
    (
        JunitOutcome::Failed,
        format!("      <failure message=\"{message}\" type=\"exit\">exit {code}</failure>\n"),
        String::new(),
        String::new(),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn empty_nodes_emits_valid_minimal_document() {
        let file = NamedTempFile::new().expect("temp file");
        write_junit_report(file.path(), &[], None).expect("write");
        let xml = std::fs::read_to_string(file.path()).expect("read");
        assert!(xml.contains("<testsuite"));
        assert!(xml.contains("tests=\"0\""));
    }

    #[test]
    fn failed_node_emits_failure_element() {
        let file = NamedTempFile::new().expect("temp file");
        let nodes = vec![NodeReport {
            name: "ci".to_owned(),
            code: Some(1),
            status: Some(NodeOutcome::Failed),
            duration_ms: Some(42),
            reason: None,
        }];
        write_junit_report(file.path(), &nodes, Some(42)).expect("write");
        let xml = std::fs::read_to_string(file.path()).expect("read");
        assert!(xml.contains("<failure"));
        assert!(xml.contains("name=\"ci\""));
        assert!(xml.contains("time=\"0.042\""));
    }
}
