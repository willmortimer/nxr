//! Shared list/inspect view filters (category + optional namespace).

use std::collections::BTreeSet;
use std::path::Path;

use nxr_core::{App, ProjectsError, listable_apps, load_projects_document};
use nxr_task::{
    TaskDefinition, TaskDocument, enrich_apps_with_listing_metadata, listable_tasks_filtered,
};

/// Resolved filter inputs for namespaced list/inspect views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewFilter {
    pub category: Option<String>,
    pub namespace: Option<String>,
    pub namespace_apps: Option<BTreeSet<String>>,
    pub namespace_tasks: Option<BTreeSet<String>>,
}

impl ViewFilter {
    /// Build a filter from CLI flags and an optional flake-root projects file.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectsError`] when `--namespace` is set but the projects file
    /// is missing/invalid, or the namespace id is unknown.
    pub fn resolve(
        flake_root: Option<&Path>,
        category: Option<&str>,
        namespace: Option<&str>,
    ) -> Result<Self, ProjectsError> {
        let category = category.map(str::to_owned);
        let Some(namespace) = namespace else {
            return Ok(Self {
                category,
                namespace: None,
                namespace_apps: None,
                namespace_tasks: None,
            });
        };

        let Some(root) = flake_root else {
            return Err(ProjectsError::MissingProjectsFile {
                filename: nxr_core::PROJECTS_FILENAME.to_owned(),
            });
        };

        let Some((path, doc)) = load_projects_document(root)? else {
            return Err(ProjectsError::MissingProjectsFile {
                filename: nxr_core::PROJECTS_FILENAME.to_owned(),
            });
        };

        doc.require_namespace(&path, namespace)?;
        Ok(Self {
            category,
            namespace: Some(namespace.to_owned()),
            namespace_apps: doc.namespace_apps(namespace),
            namespace_tasks: doc.namespace_tasks(namespace),
        })
    }

    /// Enrich apps from flake listing metadata, then apply filters.
    #[must_use]
    pub fn filter_apps(&self, apps: &[App], task_doc: &TaskDocument) -> Vec<App> {
        let mut enriched = apps.to_vec();
        enrich_apps_with_listing_metadata(&mut enriched, task_doc);
        listable_apps(
            &enriched,
            self.category.as_deref(),
            self.namespace_apps.as_ref(),
        )
    }

    /// Filter listable tasks by category and optional namespace membership.
    #[must_use]
    pub fn filter_tasks(
        &self,
        task_doc: &TaskDocument,
    ) -> std::collections::BTreeMap<String, TaskDefinition> {
        listable_tasks_filtered(
            task_doc,
            self.category.as_deref(),
            self.namespace_tasks.as_ref(),
        )
    }
}
