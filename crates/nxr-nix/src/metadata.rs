//! Optional compact `nxrMetadata.<system>` discovery endpoint.

use std::collections::BTreeMap;

use camino::Utf8Path;
use nxr_core::App;
use nxr_task::TaskDocument;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::NixError;
use crate::capabilities::NixFailureKind;
use crate::command;
use crate::eval_worker::{EvalWorkerContext, eval_json_with_worker};
use crate::tasks::{self, TaskDiscoveryError};

/// Environment variable disabling `nxrMetadata` preference (integration / bisection).
pub const NXR_METADATA_ENV: &str = "NXR_NXR_METADATA";

/// Environment variable forcing `nxrMetadata` preference on.
pub const FORCE_NXR_METADATA_ENV: &str = "NXR_FORCE_NXR_METADATA";

/// Supported major version for the `nxrMetadata` envelope.
pub const NXR_METADATA_SCHEMA_VERSION: u32 = 1;

/// Compact inventory tables embedded in `nxrMetadata`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataInventory {
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub dev_shells: Vec<String>,
}

/// Listing metadata for one app inside `nxrMetadata.apps`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct MetadataAppListing {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, rename = "workspace_path")]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub interpreter: Option<String>,
    #[serde(default, rename = "fastPath")]
    pub fast_path: Option<MetadataAppFastPath>,
    #[serde(default, rename = "runtime_path")]
    pub runtime_path: Option<String>,
}

/// Local live-workspace fast-path hint inside `nxrMetadata.apps`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct MetadataAppFastPath {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub shell: Option<String>,
}

/// Parsed `nxrMetadata.<system>` document.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct NxrMetadataDocument {
    pub schema_version: u32,
    #[serde(default = "default_task_schema_version")]
    pub task_schema_version: u32,
    #[serde(default)]
    pub apps: BTreeMap<String, MetadataAppListing>,
    #[serde(default)]
    pub tasks: BTreeMap<String, JsonValue>,
    #[serde(default)]
    pub processes: BTreeMap<String, JsonValue>,
    #[serde(default)]
    pub contexts: BTreeMap<String, JsonValue>,
    #[serde(default, rename = "discoveryInputs")]
    pub discovery_inputs: Vec<String>,
    #[serde(default)]
    pub inventory: MetadataInventory,
    #[serde(default)]
    pub namespaces: BTreeMap<String, JsonValue>,
}

fn default_task_schema_version() -> u32 {
    1
}

/// Workspace-shaped result of a successful `nxrMetadata` eval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataWorkspace {
    pub apps: Vec<App>,
    pub tasks: Option<TaskDocument>,
    pub dev_shells: Vec<String>,
    pub document: NxrMetadataDocument,
}

/// Errors while discovering or parsing `nxrMetadata`.
#[derive(Debug, thiserror::Error)]
pub enum MetadataDiscoveryError {
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error("nxrMetadata output was not valid JSON: {source}")]
    InvalidJson { source: serde_json::Error },
    #[error("unsupported nxrMetadata schema version {found}; expected major version {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
}

impl MetadataDiscoveryError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;

        match self {
            Self::Nix(error) => error.exit_code(),
            Self::InvalidJson { .. } | Self::UnsupportedVersion { .. } | Self::Tasks(_) => {
                exit::EVALUATION
            }
        }
    }
}

/// Whether discovery should attempt the optional `nxrMetadata` accelerator.
#[must_use]
pub fn nxr_metadata_preferred() -> bool {
    prefer_nxr_metadata(
        std::env::var_os(FORCE_NXR_METADATA_ENV).is_some(),
        std::env::var(NXR_METADATA_ENV).ok().as_deref(),
    )
}

#[must_use]
fn prefer_nxr_metadata(force: bool, env_value: Option<&str>) -> bool {
    if force {
        return true;
    }
    match env_value {
        Some(value) => {
            let lower = value.to_ascii_lowercase();
            !matches!(lower.as_str(), "0" | "false" | "no" | "off")
        }
        None => true,
    }
}

/// Flake installable attr path for the compact metadata document.
#[must_use]
pub fn nxr_metadata_attr_path(system: &str) -> String {
    format!("nxrMetadata.{system}")
}

/// Build argv for `nix eval --json <flake>#nxrMetadata.<system>`.
#[must_use]
pub fn nxr_metadata_eval_args(flake_ref: &str, system: &str) -> Vec<String> {
    command::flake_eval_json_args(flake_ref, &nxr_metadata_attr_path(system))
}

/// Discover `nxrMetadata.<system>` when present.
///
/// Returns `Ok(None)` when the attribute is absent so callers can fall back to
/// coalesced discovery or `flake show` + task eval.
///
/// When `worker` is set and `NXR_EVAL_WORKER=1` on an eligible host, consult the
/// experimental eval worker before spawning `nix` ([ADR-0168]).
///
/// # Errors
///
/// Returns [`MetadataDiscoveryError`] when evaluation fails for reasons other
/// than a missing attribute, or when the JSON/schema is invalid.
pub fn discover_nxr_metadata(
    nix: &Utf8Path,
    system: &str,
    args: &[String],
) -> Result<Option<NxrMetadataDocument>, MetadataDiscoveryError> {
    discover_nxr_metadata_with_worker(nix, system, args, None)
}

/// Discover `nxrMetadata.<system>` with an optional eval-worker context.
///
/// # Errors
///
/// Returns [`MetadataDiscoveryError`] when evaluation fails for reasons other
/// than a missing attribute, or when the JSON/schema is invalid.
pub fn discover_nxr_metadata_with_worker(
    nix: &Utf8Path,
    system: &str,
    args: &[String],
    worker: Option<&EvalWorkerContext>,
) -> Result<Option<NxrMetadataDocument>, MetadataDiscoveryError> {
    let attr = nxr_metadata_attr_path(system);
    let stdout = match eval_json_with_worker(nix, args, nxr_core::EvalKind::Metadata, &attr, worker)
    {
        Ok(stdout) => stdout,
        Err(error) if is_missing_nxr_metadata_attr(&error, &attr) => return Ok(None),
        Err(error) => return Err(MetadataDiscoveryError::Nix(error)),
    };
    let document = parse_nxr_metadata(&stdout)?;
    Ok(Some(document))
}

/// Parse `nxrMetadata` JSON bytes into a typed document.
///
/// # Errors
///
/// Returns [`MetadataDiscoveryError`] on JSON or schema version failures.
pub fn parse_nxr_metadata(bytes: &[u8]) -> Result<NxrMetadataDocument, MetadataDiscoveryError> {
    let document: NxrMetadataDocument = serde_json::from_slice(bytes)
        .map_err(|source| MetadataDiscoveryError::InvalidJson { source })?;
    if document.schema_version != NXR_METADATA_SCHEMA_VERSION {
        return Err(MetadataDiscoveryError::UnsupportedVersion {
            found: document.schema_version,
            expected: NXR_METADATA_SCHEMA_VERSION,
        });
    }
    Ok(document)
}

impl NxrMetadataDocument {
    /// Normalize into apps, optional tasks, and dev shell names.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataDiscoveryError`] when the embedded task document is invalid.
    pub fn into_workspace(
        self,
        flake_ref: &str,
        system: &str,
        load_tasks: bool,
    ) -> Result<MetadataWorkspace, MetadataDiscoveryError> {
        let apps = apps_from_metadata(&self, flake_ref, system);
        let dev_shells = self.inventory.dev_shells.clone();
        let task_document = self.task_document()?;
        let has_nxr_content = !task_document.tasks.is_empty()
            || !task_document.processes.is_empty()
            || !task_document.contexts.is_empty()
            || !task_document.discovery_inputs.is_empty()
            || !task_document.apps.is_empty();
        let tasks = if load_tasks {
            Some(task_document)
        } else if has_nxr_content {
            None
        } else {
            Some(TaskDocument::new(BTreeMap::new()))
        };
        Ok(MetadataWorkspace {
            apps,
            tasks,
            dev_shells,
            document: self,
        })
    }

    fn task_document(&self) -> Result<TaskDocument, MetadataDiscoveryError> {
        let mut value = serde_json::Map::new();
        value.insert(
            "schema_version".to_owned(),
            JsonValue::from(self.task_schema_version),
        );
        value.insert(
            "tasks".to_owned(),
            JsonValue::Object(
                self.tasks
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
        if !self.apps.is_empty() {
            let apps = self
                .apps
                .iter()
                .filter_map(|(name, listing)| {
                    let mut meta = serde_json::Map::new();
                    if let Some(category) = listing.category.as_ref() {
                        meta.insert("category".to_owned(), JsonValue::String(category.clone()));
                    }
                    if let Some(path) = listing.workspace_path.as_ref() {
                        meta.insert("workspace_path".to_owned(), JsonValue::String(path.clone()));
                    }
                    if let Some(interpreter) = listing.interpreter.as_ref() {
                        meta.insert(
                            "interpreter".to_owned(),
                            JsonValue::String(interpreter.clone()),
                        );
                    }
                    if let Some(fast_path) = listing.fast_path.as_ref() {
                        let mut fp = serde_json::Map::new();
                        fp.insert("enable".to_owned(), JsonValue::Bool(fast_path.enable));
                        if let Some(shell) = fast_path.shell.as_ref() {
                            fp.insert("shell".to_owned(), JsonValue::String(shell.clone()));
                        }
                        meta.insert("fastPath".to_owned(), JsonValue::Object(fp));
                    }
                    if let Some(runtime_path) = listing.runtime_path.as_ref() {
                        meta.insert(
                            "runtime_path".to_owned(),
                            JsonValue::String(runtime_path.clone()),
                        );
                    }
                    if meta.is_empty() {
                        None
                    } else {
                        Some((name.clone(), JsonValue::Object(meta)))
                    }
                })
                .collect::<serde_json::Map<_, _>>();
            if !apps.is_empty() {
                value.insert("apps".to_owned(), JsonValue::Object(apps));
            }
        }
        if !self.contexts.is_empty() {
            value.insert(
                "contexts".to_owned(),
                JsonValue::Object(
                    self.contexts
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            );
        }
        if !self.processes.is_empty() {
            value.insert(
                "processes".to_owned(),
                JsonValue::Object(
                    self.processes
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            );
        }
        if !self.discovery_inputs.is_empty() {
            value.insert(
                "discoveryInputs".to_owned(),
                JsonValue::from(self.discovery_inputs.clone()),
            );
        }
        tasks::parse_task_document(&JsonValue::Object(value)).map_err(MetadataDiscoveryError::Tasks)
    }
}

fn apps_from_metadata(doc: &NxrMetadataDocument, flake_ref: &str, system: &str) -> Vec<App> {
    let mut names = doc.inventory.apps.clone();
    if names.is_empty() {
        names.extend(doc.apps.keys().cloned());
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            let listing = doc.apps.get(&name);
            let mut metadata = BTreeMap::new();
            if let Some(category) = listing.and_then(|entry| entry.category.clone()) {
                metadata.insert(
                    nxr_core::NXR_CATEGORY_KEY.to_owned(),
                    JsonValue::String(category),
                );
            }
            App {
                is_default: name == "default",
                description: listing.and_then(|entry| entry.description.clone()),
                attr_path: format!("apps.{system}.{name}"),
                flake_ref: flake_ref.to_owned(),
                system: system.to_owned(),
                name,
                metadata,
            }
        })
        .collect()
}

fn is_missing_nxr_metadata_attr(error: &NixError, attr_path: &str) -> bool {
    let NixError::CommandFailed {
        stderr,
        kind: NixFailureKind::Evaluation,
        ..
    } = error
    else {
        return false;
    };

    let lower = stderr.to_ascii_lowercase();
    let mentions = lower.contains("nxrmetadata") || lower.contains(&attr_path.to_ascii_lowercase());
    if !mentions {
        return false;
    }

    lower.contains("does not provide attribute")
        || lower.contains("missing attribute")
        || lower.contains("attribute 'nxrmetadata' missing")
        || lower.contains("does not contain")
}

#[cfg(test)]
mod tests {
    use super::{
        NXR_METADATA_SCHEMA_VERSION, discover_nxr_metadata, is_missing_nxr_metadata_attr,
        nxr_metadata_attr_path, nxr_metadata_eval_args, parse_nxr_metadata, prefer_nxr_metadata,
    };
    use crate::NixError;
    use crate::OptionalNixFlags;
    use crate::capabilities::NixFailureKind;
    use camino::Utf8PathBuf;
    use serde_json::json;

    #[test]
    fn attr_path_and_args() {
        assert_eq!(
            nxr_metadata_attr_path("aarch64-darwin"),
            "nxrMetadata.aarch64-darwin"
        );
        let args = nxr_metadata_eval_args(".", "x86_64-linux");
        assert_eq!(args[0], "eval");
        assert_eq!(args[1], "--json");
        assert_eq!(args[2], ".#nxrMetadata.x86_64-linux");
    }

    #[test]
    fn preferred_respects_kill_switch() {
        assert!(prefer_nxr_metadata(false, None));
        assert!(prefer_nxr_metadata(true, Some("off")));
        assert!(!prefer_nxr_metadata(false, Some("off")));
        assert!(!prefer_nxr_metadata(false, Some("0")));
        assert!(prefer_nxr_metadata(false, Some("1")));
    }

    #[test]
    fn parse_envelope_and_into_workspace() {
        let raw = serde_json::to_vec(&json!({
            "schema_version": NXR_METADATA_SCHEMA_VERSION,
            "task_schema_version": 1,
            "apps": {
                "hello": { "description": "Say hello", "category": "demo" }
            },
            "tasks": {
                "ci": {
                    "app": "hello",
                    "dependsOn": [],
                    "hidden": false
                }
            },
            "inventory": {
                "apps": ["hello"],
                "devShells": ["default"]
            },
            "namespaces": {
                "demo": { "apps": ["hello"], "tasks": ["ci"] }
            }
        }))
        .expect("serialize");
        let doc = parse_nxr_metadata(&raw).expect("parse");
        let workspace = doc
            .into_workspace("path:/tmp/flake", "aarch64-darwin", true)
            .expect("workspace");
        assert_eq!(workspace.apps.len(), 1);
        assert_eq!(workspace.apps[0].name, "hello");
        assert_eq!(workspace.apps[0].description.as_deref(), Some("Say hello"));
        assert_eq!(workspace.dev_shells, vec!["default".to_owned()]);
        assert!(
            workspace
                .tasks
                .as_ref()
                .is_some_and(|tasks| tasks.tasks.contains_key("ci"))
        );
        assert!(!workspace.document.namespaces.is_empty());
    }

    #[test]
    fn rejects_unsupported_metadata_schema() {
        let raw = serde_json::to_vec(&json!({
            "schema_version": 99,
            "tasks": {},
            "inventory": { "apps": [] }
        }))
        .expect("serialize");
        let err = parse_nxr_metadata(&raw).expect_err("version");
        assert!(matches!(
            err,
            super::MetadataDiscoveryError::UnsupportedVersion { found: 99, .. }
        ));
    }

    #[test]
    fn missing_attr_detection() {
        let error = NixError::CommandFailed {
            nix: Utf8PathBuf::from("/nix/var/nix/profiles/default/bin/nix"),
            args: vec!["eval".into()],
            status: Some(1),
            stderr:
                "error: flake 'path:/tmp/x' does not provide attribute 'nxrMetadata.aarch64-darwin'"
                    .into(),
            kind: NixFailureKind::Evaluation,
        };
        assert!(is_missing_nxr_metadata_attr(
            &error,
            "nxrMetadata.aarch64-darwin"
        ));
    }

    #[test]
    fn discover_nxr_metadata_fixture() {
        if std::env::var_os("NXR_SKIP_NIX_INTEGRATION").is_some() {
            return;
        }
        let Some(nix_path) = which::which("nix").ok() else {
            eprintln!("skipping: nix not on PATH");
            return;
        };
        let nix = Utf8PathBuf::from_path_buf(nix_path).expect("utf-8 path");
        let adapter = crate::NixAdapter::new().expect("adapter");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../fixtures/nxr-metadata");
        let flake_ref = format!(
            "path:{}",
            fixture.canonicalize().expect("fixture").display()
        );
        let args = nxr_metadata_eval_args(&flake_ref, &adapter.system);
        let flags = OptionalNixFlags {
            no_write_lock_file: true,
            ..Default::default()
        };
        let args = adapter
            .compatible_argv(args, &flags)
            .expect("compatible argv");
        let document = discover_nxr_metadata(&nix, &adapter.system, &args)
            .expect("discover")
            .expect("metadata present");
        let workspace = document
            .into_workspace(&flake_ref, &adapter.system, true)
            .expect("workspace");
        assert!(workspace.apps.iter().any(|app| app.name == "hello"));
        assert!(
            workspace
                .tasks
                .as_ref()
                .is_some_and(|doc| doc.tasks.contains_key("ci"))
        );
        assert!(workspace.document.namespaces.contains_key("demo"));
    }
}
