//! Optional non-authoritative project/namespace views.
//!
//! [`ProjectsDocument`] may live at the flake root as `nxr.projects.json`.
//! It never invents flake apps or tasks: filters and grouping only reference
//! names that must already exist as flake outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Supported major version for the optional projects document.
pub const PROJECTS_SCHEMA_VERSION: u32 = 1;

/// Well-known filename at the flake root (optional).
pub const PROJECTS_FILENAME: &str = "nxr.projects.json";

/// Metadata key on [`crate::App::metadata`] for listing category.
pub const NXR_CATEGORY_KEY: &str = "nxr.category";

/// Errors while loading or validating an optional projects document.
#[derive(Debug, Error)]
pub enum ProjectsError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("projects metadata in {path} was not valid JSON: {source}")]
    InvalidJson {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("projects metadata in {path} did not match the projects schema: {source}")]
    InvalidDocument {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "unsupported projects schema version {found} in {path}; expected major version {expected}"
    )]
    UnsupportedVersion {
        path: String,
        found: u32,
        expected: u32,
    },
    #[error("unknown project namespace `{name}` in {path}")]
    UnknownNamespace { path: String, name: String },
    #[error(
        "--namespace requires optional {filename} at the flake root (non-authoritative view metadata)"
    )]
    MissingProjectsFile { filename: String },
}

impl ProjectsError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use crate::diagnostics::exit;
        match self {
            Self::Io { .. }
            | Self::InvalidJson { .. }
            | Self::InvalidDocument { .. }
            | Self::UnsupportedVersion { .. }
            | Self::UnknownNamespace { .. }
            | Self::MissingProjectsFile { .. } => exit::INVALID_METADATA,
        }
    }
}

/// Versioned optional project/namespace map for list/inspect views.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectsDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectDefinition>,
}

/// One named project/namespace used only for filtering and grouping.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional category label mirrored onto member apps for `--category`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Flake app leaf names (`apps.<system>.<name>`) in this namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    /// Task names in this namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
}

/// Kind of namespace member referenced in [`ProjectsDocument`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ProjectMemberKind {
    App,
    Task,
}

/// A project namespace member that does not exist in the flake catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownProjectMember {
    pub namespace: String,
    pub member: String,
    pub kind: ProjectMemberKind,
}

impl ProjectsDocument {
    pub const SCHEMA_VERSION: u32 = PROJECTS_SCHEMA_VERSION;

    #[must_use]
    pub fn new(projects: BTreeMap<String, ProjectDefinition>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            projects,
        }
    }

    /// Validate schema version.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectsError::UnsupportedVersion`] when the major version is
    /// not supported.
    pub fn validate(&self, path: &str) -> Result<(), ProjectsError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ProjectsError::UnsupportedVersion {
                path: path.to_owned(),
                found: self.schema_version,
                expected: Self::SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// Member app names for `namespace`, if defined.
    #[must_use]
    pub fn namespace_apps(&self, namespace: &str) -> Option<BTreeSet<String>> {
        self.projects
            .get(namespace)
            .map(|project| project.apps.iter().cloned().collect())
    }

    /// Member task names for `namespace`, if defined.
    #[must_use]
    pub fn namespace_tasks(&self, namespace: &str) -> Option<BTreeSet<String>> {
        self.projects
            .get(namespace)
            .map(|project| project.tasks.iter().cloned().collect())
    }

    /// Look up a project; error when the namespace is unknown.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectsError::UnknownNamespace`] when `namespace` is not a key.
    pub fn require_namespace(
        &self,
        path: &str,
        namespace: &str,
    ) -> Result<&ProjectDefinition, ProjectsError> {
        self.projects
            .get(namespace)
            .ok_or_else(|| ProjectsError::UnknownNamespace {
                path: path.to_owned(),
                name: namespace.to_owned(),
            })
    }

    /// Members listed in project namespaces that are absent from the flake catalog.
    #[must_use]
    pub fn unknown_members(
        &self,
        known_apps: &BTreeSet<String>,
        known_tasks: &BTreeSet<String>,
    ) -> Vec<UnknownProjectMember> {
        let mut unknown = Vec::new();
        for (namespace, project) in &self.projects {
            for app in &project.apps {
                if !known_apps.contains(app) {
                    unknown.push(UnknownProjectMember {
                        namespace: namespace.clone(),
                        member: app.clone(),
                        kind: ProjectMemberKind::App,
                    });
                }
            }
            for task in &project.tasks {
                if !known_tasks.contains(task) {
                    unknown.push(UnknownProjectMember {
                        namespace: namespace.clone(),
                        member: task.clone(),
                        kind: ProjectMemberKind::Task,
                    });
                }
            }
        }
        unknown.sort_by(|left, right| {
            (&left.namespace, left.kind, &left.member).cmp(&(
                &right.namespace,
                right.kind,
                &right.member,
            ))
        });
        unknown
    }
}

/// Load `nxr.projects.json` from `flake_root` when present.
///
/// Missing file → `Ok(None)`. Present but invalid → error.
///
/// # Errors
///
/// Returns [`ProjectsError`] when the file exists but cannot be read or parsed.
pub fn load_projects_document(
    flake_root: &std::path::Path,
) -> Result<Option<(String, ProjectsDocument)>, ProjectsError> {
    let path = flake_root.join(PROJECTS_FILENAME);
    if !path.is_file() {
        return Ok(None);
    }
    let path_display = path.display().to_string();
    let bytes = fs::read(&path).map_err(|source| ProjectsError::Io {
        path: path_display.clone(),
        source,
    })?;
    let doc: ProjectsDocument =
        serde_json::from_slice(&bytes).map_err(|source| ProjectsError::InvalidDocument {
            path: path_display.clone(),
            source,
        })?;
    doc.validate(&path_display)?;
    Ok(Some((path_display, doc)))
}

/// Read category from app metadata (`nxr.category`).
#[must_use]
pub fn app_category(app: &crate::App) -> Option<&str> {
    app.metadata
        .get(NXR_CATEGORY_KEY)
        .and_then(serde_json::Value::as_str)
}

/// Set `nxr.category` on an app when `category` is present.
pub fn set_app_category(app: &mut crate::App, category: Option<&str>) {
    match category {
        Some(value) if !value.is_empty() => {
            app.metadata.insert(
                NXR_CATEGORY_KEY.to_owned(),
                serde_json::Value::String(value.to_owned()),
            );
        }
        _ => {}
    }
}

/// Filter discovered apps by optional category and/or namespace membership.
#[must_use]
pub fn listable_apps<'a>(
    apps: impl IntoIterator<Item = &'a crate::App>,
    category: Option<&str>,
    namespace_apps: Option<&BTreeSet<String>>,
) -> Vec<crate::App> {
    apps.into_iter()
        .filter(|app| category.is_none_or(|cat| app_category(app) == Some(cat)))
        .filter(|app| namespace_apps.is_none_or(|members| members.contains(&app.name)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        NXR_CATEGORY_KEY, ProjectDefinition, ProjectMemberKind, ProjectsDocument, UnknownProjectMember,
        app_category, listable_apps, load_projects_document, set_app_category,
    };
    use crate::App;

    fn sample_app(name: &str, category: Option<&str>) -> App {
        let mut app = App {
            name: name.to_owned(),
            attr_path: format!("apps.aarch64-darwin.{name}"),
            flake_ref: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: None,
            is_default: false,
            metadata: BTreeMap::new(),
        };
        set_app_category(&mut app, category);
        app
    }

    #[test]
    fn projects_document_round_trip() {
        let mut projects = BTreeMap::new();
        projects.insert(
            "api".to_owned(),
            ProjectDefinition {
                description: Some("API package".to_owned()),
                category: Some("backend".to_owned()),
                apps: vec!["api-test".to_owned(), "api-lint".to_owned()],
                tasks: vec!["api-ci".to_owned()],
            },
        );
        let doc = ProjectsDocument::new(projects);
        let value = serde_json::to_value(&doc).expect("serialize");
        assert_eq!(value["schema_version"], json!(1));
        let restored: ProjectsDocument = serde_json::from_value(value).expect("deserialize");
        assert_eq!(restored, doc);
    }

    #[test]
    fn load_missing_projects_is_none() {
        let temp = TempDir::new().expect("tempdir");
        assert!(load_projects_document(temp.path()).expect("load").is_none());
    }

    #[test]
    fn load_projects_document_reads_file() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("nxr.projects.json");
        std::fs::write(
            &path,
            r#"{
              "schema_version": 1,
              "projects": {
                "web": { "apps": ["web-test"], "tasks": ["web-ci"] }
              }
            }"#,
        )
        .expect("write");
        let (loaded_path, doc) = load_projects_document(temp.path())
            .expect("load")
            .expect("present");
        assert!(loaded_path.ends_with("nxr.projects.json"));
        assert_eq!(
            doc.namespace_apps("web"),
            Some(BTreeSet::from(["web-test".to_owned()]))
        );
        assert_eq!(
            doc.namespace_tasks("web"),
            Some(BTreeSet::from(["web-ci".to_owned()]))
        );
    }

    #[test]
    fn listable_apps_filter_category_and_namespace() {
        let apps = vec![
            sample_app("api-test", Some("backend")),
            sample_app("web-test", Some("frontend")),
            sample_app("shared", None),
        ];
        let filtered = listable_apps(&apps, Some("backend"), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "api-test");
        assert_eq!(app_category(&filtered[0]), Some("backend"));
        assert_eq!(
            filtered[0].metadata.get(NXR_CATEGORY_KEY),
            Some(&json!("backend"))
        );

        let members = BTreeSet::from(["web-test".to_owned()]);
        let namespaced = listable_apps(&apps, None, Some(&members));
        assert_eq!(namespaced.len(), 1);
        assert_eq!(namespaced[0].name, "web-test");
    }

    #[test]
    fn unknown_members_reports_missing_apps_and_tasks() {
        let mut projects = BTreeMap::new();
        projects.insert(
            "api".to_owned(),
            ProjectDefinition {
                apps: vec!["known-app".to_owned(), "ghost-app".to_owned()],
                tasks: vec!["known-task".to_owned(), "ghost-task".to_owned()],
                ..ProjectDefinition::default()
            },
        );
        let doc = ProjectsDocument::new(projects);
        let known_apps = BTreeSet::from(["known-app".to_owned()]);
        let known_tasks = BTreeSet::from(["known-task".to_owned()]);
        assert_eq!(
            doc.unknown_members(&known_apps, &known_tasks),
            vec![
                UnknownProjectMember {
                    namespace: "api".to_owned(),
                    member: "ghost-app".to_owned(),
                    kind: ProjectMemberKind::App,
                },
                UnknownProjectMember {
                    namespace: "api".to_owned(),
                    member: "ghost-task".to_owned(),
                    kind: ProjectMemberKind::Task,
                },
            ]
        );
    }
}
