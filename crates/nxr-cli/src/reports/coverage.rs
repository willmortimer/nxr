//! Coverage JSON report stub (valid empty document when no data).

use std::path::Path;

use serde_json::json;

use super::{ReportWriteError, write_json_value};

/// Write a minimal valid coverage JSON document with no file entries.
///
/// # Errors
///
/// Returns [`ReportWriteError`] when the file cannot be written.
pub fn write_coverage_report(path: &Path) -> Result<(), ReportWriteError> {
    let document = json!({
        "schema_version": 1,
        "format": "nxr-coverage-v1",
        "files": []
    });
    write_json_value(path, &document)
}
