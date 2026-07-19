//! Nix CLI adapter: executable discovery, system detection, and app listing.

use camino::Utf8PathBuf;

use crate::NixError;
use crate::capabilities::{detect_system, locate_nix};
use crate::discovery;
use crate::tasks::{self, TaskDiscoveryError};
use nxr_core::App;
use nxr_task::TaskDocument;

/// Configured Nix CLI adapter for the current host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NixAdapter {
    /// Resolved `nix` executable path.
    pub nix: Utf8PathBuf,
    /// Current Nix system string (`builtins.currentSystem`).
    pub system: String,
}

impl NixAdapter {
    /// Locate `nix`, detect the current system, and return a ready adapter.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when `nix` cannot be located or the current system cannot be detected.
    pub fn new() -> Result<Self, NixError> {
        let nix = locate_nix()?;
        let system = detect_system(&nix)?;
        Ok(Self { nix, system })
    }

    /// Construct an adapter from known paths (primarily for tests).
    #[must_use]
    pub const fn with_nix_and_system(nix: Utf8PathBuf, system: String) -> Self {
        Self { nix, system }
    }

    /// Discover flake apps for the adapter's current system.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when `nix flake show` fails or its JSON cannot be parsed.
    pub fn discover_apps(&self, flake_ref: &str) -> Result<Vec<App>, NixError> {
        discovery::discover_apps(&self.nix, &self.system, flake_ref)
    }

    /// Discover versioned task metadata (`nxr.<system>`) for the current system.
    ///
    /// Missing `nxr` output yields an empty [`TaskDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`TaskDiscoveryError`] when evaluation or schema validation fails.
    pub fn discover_tasks(&self, flake_ref: &str) -> Result<TaskDocument, TaskDiscoveryError> {
        tasks::discover_tasks(&self.nix, &self.system, flake_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::NixAdapter;
    use crate::NixError;
    use crate::capabilities::NixFailureKind;
    use camino::Utf8PathBuf;
    use nxr_core::diagnostics::exit;

    #[test]
    #[ignore = "requires nix and fixture flakes"]
    fn discover_apps_from_basic_apps_fixture() {
        let adapter = NixAdapter::new().expect("adapter");
        let apps = adapter
            .discover_apps("./fixtures/basic-apps")
            .expect("discover apps");

        assert!(!apps.is_empty());
        assert!(apps.windows(2).all(|pair| pair[0].name <= pair[1].name));
        assert!(
            apps.iter()
                .any(|app| app.name == "default" && app.is_default)
        );
        assert!(apps.iter().any(|app| app.name == "hello"));
    }

    #[test]
    #[ignore = "requires nix and fixture flakes"]
    fn discover_tasks_from_task_dag_fixture() {
        let adapter = NixAdapter::new().expect("adapter");
        let fixture = fixture_path("task-dag");
        let doc = adapter
            .discover_tasks(fixture.as_str())
            .expect("discover tasks");

        assert_eq!(doc.schema_version, nxr_task::SCHEMA_VERSION);
        assert_eq!(doc.tasks.len(), 3);
        assert_eq!(
            doc.tasks.get("test").expect("test").depends_on,
            vec!["fmt".to_owned()]
        );
        assert_eq!(
            doc.tasks.get("ci").expect("ci").depends_on,
            vec!["test".to_owned()]
        );
    }

    #[test]
    #[ignore = "requires nix and fixture flakes"]
    fn discover_tasks_missing_attr_is_empty() {
        let adapter = NixAdapter::new().expect("adapter");
        let fixture = fixture_path("basic-apps");
        let doc = adapter
            .discover_tasks(fixture.as_str())
            .expect("missing nxr output is empty");
        assert!(doc.tasks.is_empty());
        assert_eq!(doc.schema_version, nxr_task::SCHEMA_VERSION);
    }

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .join("../../fixtures")
            .join(name)
            .canonicalize_utf8()
            .unwrap_or_else(|_| manifest.join("../../fixtures").join(name))
    }

    #[test]
    fn nix_not_found_maps_to_capability_exit_code() {
        let error = NixError::NixNotFound {
            path: camino::Utf8PathBuf::from("/no/such/nix"),
        };
        assert_eq!(error.exit_code(), exit::NIX_CAPABILITY);
    }

    #[test]
    fn flake_show_failure_maps_to_evaluation_exit_code() {
        let error = NixError::CommandFailed {
            nix: camino::Utf8PathBuf::from("/bin/nix"),
            args: vec!["flake".to_owned(), "show".to_owned()],
            status: Some(1),
            stderr: "error".to_owned(),
            kind: NixFailureKind::Evaluation,
        };
        assert_eq!(error.exit_code(), exit::EVALUATION);
        assert!(error.user_message().contains("failed to evaluate flake"));
        assert!(error.user_message().contains("nix flake show"));
    }
}
