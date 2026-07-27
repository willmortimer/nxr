//! SARIF report writer for failed task nodes.

use std::path::Path;

use nxr_task::NodeOutcome;
use serde_json::json;

use super::{NodeReport, ReportWriteError, write_json_value};

/// Write a SARIF 2.1.0 report with one result per failed node.
///
/// When there are no failures, emits a valid empty `results` array.
///
/// # Errors
///
/// Returns [`ReportWriteError`] when the file cannot be written.
pub fn write_sarif_report(path: &Path, nodes: &[NodeReport]) -> Result<(), ReportWriteError> {
    let version = env!("CARGO_PKG_VERSION");
    let results: Vec<serde_json::Value> = nodes
        .iter()
        .filter(|node| is_failure(node))
        .map(|node| {
            let message = node
                .reason
                .clone()
                .or_else(|| node.code.map(|code| format!("exit {code}")))
                .unwrap_or_else(|| "task failed".to_owned());
            json!({
                "ruleId": node.name,
                "level": "error",
                "message": { "text": message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": format!("task://{}", node.name) }
                    }
                }]
            })
        })
        .collect();

    let document = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "nxr",
                    "version": version,
                    "informationUri": "https://github.com/nxr-dev/nxr"
                }
            },
            "results": results
        }]
    });
    write_json_value(path, &document)
}

fn is_failure(node: &NodeReport) -> bool {
    match node.status {
        Some(NodeOutcome::Failed | NodeOutcome::TimedOut) => true,
        Some(NodeOutcome::Succeeded | NodeOutcome::Skipped | NodeOutcome::Cancelled) => false,
        None => node.code.is_some_and(|code| code != 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn no_failures_emits_empty_results() {
        let file = NamedTempFile::new().expect("temp file");
        let nodes = vec![NodeReport {
            name: "ok".to_owned(),
            code: Some(0),
            status: Some(NodeOutcome::Succeeded),
            duration_ms: Some(1),
            reason: None,
        }];
        write_sarif_report(file.path(), &nodes).expect("write");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file.path()).expect("read"))
                .expect("json");
        assert!(
            value["runs"][0]["results"]
                .as_array()
                .is_some_and(|r| r.is_empty())
        );
    }
}
