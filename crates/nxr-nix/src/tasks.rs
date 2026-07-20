//! Task metadata discovery via `nix eval --json` on `nxr.<system>`.

use camino::Utf8Path;
use nxr_task::{SchemaError, TaskDocument};
use serde_json::Value as JsonValue;

use crate::NixError;
use crate::capabilities::{NixFailureKind, run_nix};
use crate::command;

/// Errors while discovering or parsing flake task metadata.
#[derive(Debug)]
pub enum TaskDiscoveryError {
    /// Underlying Nix adapter failure (spawn, eval, capability, …).
    Nix(NixError),

    /// `nix eval --json` stdout was not valid JSON.
    InvalidJson { source: serde_json::Error },

    /// JSON did not deserialize into a [`TaskDocument`].
    InvalidDocument { source: serde_json::Error },

    /// Document major version is not supported.
    Schema(SchemaError),
}

impl std::error::Error for TaskDiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nix(error) => Some(error),
            Self::InvalidJson { source } | Self::InvalidDocument { source } => Some(source),
            Self::Schema(error) => Some(error),
        }
    }
}

impl std::fmt::Display for TaskDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl From<NixError> for TaskDiscoveryError {
    fn from(error: NixError) -> Self {
        Self::Nix(error)
    }
}

impl From<SchemaError> for TaskDiscoveryError {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

impl TaskDiscoveryError {
    /// User-facing message with sanitized Nix context when present.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Nix(error) => error.user_message(),
            Self::InvalidJson { source } => {
                format!("nix task metadata was not valid JSON: {source}")
            }
            Self::InvalidDocument { source } => {
                format!("nix task metadata did not match the task document schema: {source}")
            }
            Self::Schema(error) => error.to_string(),
        }
    }

    /// Stable `nxr` exit code for this discovery error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;

        match self {
            Self::Nix(error) => error.exit_code(),
            Self::InvalidJson { .. } | Self::InvalidDocument { .. } | Self::Schema(_) => {
                exit::EVALUATION
            }
        }
    }
}

/// Flake installable attr path for the versioned task document.
///
/// Documents the contract `nxr.<system>` (see `docs/TASKS.md`).
#[must_use]
pub fn tasks_attr_path(system: &str) -> String {
    format!("nxr.{system}")
}

/// Discover tasks for `system` from `flake_ref` via `nix eval --json`.
///
/// Missing `nxr.<system>` (or the parent `nxr` output) yields an empty
/// [`TaskDocument`]. Unsupported schema majors return
/// [`TaskDiscoveryError::Schema`].
///
/// # Errors
///
/// Returns [`TaskDiscoveryError`] when Nix evaluation fails for reasons other
/// than a missing tasks attribute, or when the JSON/schema is invalid.
pub fn discover_tasks(
    nix: &Utf8Path,
    system: &str,
    flake_ref: &str,
) -> Result<TaskDocument, TaskDiscoveryError> {
    let attr = tasks_attr_path(system);
    let args = command::flake_eval_json_args(flake_ref, &attr);
    discover_tasks_with_args(nix, system, &args)
}

/// Discover tasks using a pre-built (capability-aware) argv.
///
/// # Errors
///
/// Returns [`TaskDiscoveryError`] when Nix evaluation fails for reasons other
/// than a missing tasks attribute, or when the JSON/schema is invalid.
pub fn discover_tasks_with_args(
    nix: &Utf8Path,
    system: &str,
    args: &[String],
) -> Result<TaskDocument, TaskDiscoveryError> {
    let attr = tasks_attr_path(system);
    let stdout = match run_nix(nix, args, NixFailureKind::Evaluation) {
        Ok(stdout) => stdout,
        Err(error) if is_missing_nxr_attr(&error, &attr) => {
            return Ok(TaskDocument::new(std::collections::BTreeMap::new()));
        }
        Err(error) => return Err(TaskDiscoveryError::Nix(error)),
    };

    let value: JsonValue = serde_json::from_slice(&stdout)
        .map_err(|source| TaskDiscoveryError::InvalidJson { source })?;
    parse_task_document(&value)
}

/// Parse a [`TaskDocument`] from JSON (for example `nix eval --json` output).
///
/// # Errors
///
/// Returns [`TaskDiscoveryError::InvalidDocument`] on serde failures and
/// [`TaskDiscoveryError::Schema`] when the major version is unsupported.
pub fn parse_task_document(value: &JsonValue) -> Result<TaskDocument, TaskDiscoveryError> {
    let doc: TaskDocument = serde_json::from_value(value.clone())
        .map_err(|source| TaskDiscoveryError::InvalidDocument { source })?;
    doc.validate()?;
    Ok(doc)
}

/// Whether a Nix evaluation error indicates the `nxr.<system>` attr is absent.
fn is_missing_nxr_attr(error: &NixError, attr_path: &str) -> bool {
    let NixError::CommandFailed {
        stderr,
        kind: NixFailureKind::Evaluation,
        ..
    } = error
    else {
        return false;
    };

    let lower = stderr.to_ascii_lowercase();
    let mentions_nxr = lower.contains("nxr") || lower.contains(&attr_path.to_ascii_lowercase());
    if !mentions_nxr {
        return false;
    }

    lower.contains("does not provide attribute")
        || lower.contains("missing attribute")
        || lower.contains("attribute 'nxr' missing")
        || lower.contains("does not contain")
}

#[cfg(test)]
mod tests {
    use super::{TaskDiscoveryError, is_missing_nxr_attr, parse_task_document, tasks_attr_path};
    use crate::NixError;
    use crate::capabilities::NixFailureKind;
    use camino::Utf8PathBuf;
    use nxr_core::diagnostics::exit;
    use nxr_task::{SCHEMA_VERSION, SchemaError};
    use serde_json::json;

    const TASK_DAG_METADATA: &str =
        include_str!("../../../tests/fixtures/task-dag-nxr-metadata.json");

    #[test]
    fn tasks_attr_path_matches_documented_contract() {
        assert_eq!(tasks_attr_path("aarch64-darwin"), "nxr.aarch64-darwin");
        assert_eq!(tasks_attr_path("x86_64-linux"), "nxr.x86_64-linux");
    }

    #[test]
    fn parse_golden_task_dag_fixture() {
        let value: serde_json::Value =
            serde_json::from_str(TASK_DAG_METADATA).expect("parse golden JSON");
        let doc = parse_task_document(&value).expect("parse task document");

        assert_eq!(doc.schema_version, SCHEMA_VERSION);
        assert_eq!(doc.tasks.len(), 3);

        let fmt = doc.tasks.get("fmt").expect("fmt");
        assert_eq!(fmt.app, "fmt");
        assert!(fmt.depends_on.is_empty());

        let test = doc.tasks.get("test").expect("test");
        assert_eq!(test.depends_on, vec!["fmt".to_owned()]);

        let ci = doc.tasks.get("ci").expect("ci");
        assert_eq!(ci.depends_on, vec!["test".to_owned()]);
        assert_eq!(ci.category.as_deref(), Some("validation"));
    }

    #[test]
    fn parse_empty_tasks_is_ok() {
        let value = json!({
            "schema_version": 1,
            "tasks": {}
        });
        let doc = parse_task_document(&value).expect("empty tasks ok");
        assert!(doc.tasks.is_empty());
    }

    #[test]
    fn parse_rejects_unsupported_schema_major() {
        let value = json!({
            "schema_version": 99,
            "tasks": {}
        });
        let err = parse_task_document(&value).expect_err("unsupported major");
        assert!(matches!(
            err,
            TaskDiscoveryError::Schema(SchemaError::UnsupportedVersion {
                found: 99,
                expected: 1
            })
        ));
        assert_eq!(err.exit_code(), exit::EVALUATION);
    }

    #[test]
    fn parse_rejects_missing_app_field() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "ci": { "dependsOn": ["test"] }
            }
        });
        let err = parse_task_document(&value).expect_err("app required");
        assert!(matches!(err, TaskDiscoveryError::InvalidDocument { .. }));
    }

    #[test]
    fn parse_rejects_absolute_working_directory() {
        let value = json!({
            "schema_version": 1,
            "tasks": {
                "fmt": {
                    "app": "fmt",
                    "workingDirectory": "/absolute"
                }
            }
        });
        let err = parse_task_document(&value).expect_err("absolute path rejected");
        assert!(matches!(
            err,
            TaskDiscoveryError::Schema(SchemaError::AbsoluteWorkingDirectory { .. })
        ));
        assert_eq!(err.exit_code(), exit::EVALUATION);
    }

    #[test]
    fn missing_nxr_attr_detection() {
        let error = NixError::CommandFailed {
            nix: Utf8PathBuf::from("nix"),
            args: vec!["eval".to_owned()],
            status: Some(1),
            stderr:
                "error: flake 'git+file:///tmp/x' does not provide attribute 'nxr.aarch64-darwin'"
                    .to_owned(),
            kind: NixFailureKind::Evaluation,
        };
        assert!(is_missing_nxr_attr(&error, "nxr.aarch64-darwin"));

        let other = NixError::CommandFailed {
            nix: Utf8PathBuf::from("nix"),
            args: vec!["eval".to_owned()],
            status: Some(1),
            stderr: "error: infinite recursion encountered".to_owned(),
            kind: NixFailureKind::Evaluation,
        };
        assert!(!is_missing_nxr_attr(&other, "nxr.aarch64-darwin"));
    }
}
