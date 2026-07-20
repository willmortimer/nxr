//! Nix CLI adapter: executable discovery, capability negotiation, and app listing.

use camino::Utf8PathBuf;

use crate::NixError;
use crate::capabilities::{
    NixCapabilities, OptionalNixFlags, detect_capabilities, detect_system, locate_nix,
};
use crate::command;
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
    /// Negotiated CLI capabilities (detected once at construction).
    pub capabilities: NixCapabilities,
}

impl NixAdapter {
    /// Locate `nix`, detect system + capabilities, and return a ready adapter.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when `nix` cannot be located, the current system
    /// cannot be detected, or capability probing fails.
    pub fn new() -> Result<Self, NixError> {
        let nix = locate_nix()?;
        Self::from_nix(nix)
    }

    /// Build an adapter for an explicit `nix` executable path.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when system or capability detection fails.
    pub fn from_nix(nix: Utf8PathBuf) -> Result<Self, NixError> {
        let system = detect_system(&nix)?;
        let capabilities = detect_capabilities(&nix)?;
        Ok(Self {
            nix,
            system,
            capabilities,
        })
    }

    /// Construct an adapter from known parts (primarily for tests).
    #[must_use]
    pub const fn with_parts(
        nix: Utf8PathBuf,
        system: String,
        capabilities: NixCapabilities,
    ) -> Self {
        Self {
            nix,
            system,
            capabilities,
        }
    }

    /// Construct an adapter with assumed modern capabilities (unit tests).
    #[must_use]
    pub fn with_nix_and_system(nix: Utf8PathBuf, system: String) -> Self {
        Self::with_parts(
            nix,
            system,
            NixCapabilities::all_supported_for_tests(crate::capabilities::TESTED_NIX_SUPPORT_FLOOR),
        )
    }

    /// Fail when flakes are not enabled on this Nix.
    ///
    /// # Errors
    ///
    /// Returns [`NixError::FlakesDisabled`] when experimental flakes are off.
    pub fn require_flakes(&self) -> Result<(), NixError> {
        if self.capabilities.flakes_enabled {
            Ok(())
        } else {
            Err(NixError::FlakesDisabled {
                version: self.capabilities.version,
            })
        }
    }

    /// Choose a compatible argv for the requested optional flags.
    #[must_use]
    pub fn compatible_argv(
        &self,
        base_args: Vec<String>,
        requested: &OptionalNixFlags,
    ) -> Vec<String> {
        self.capabilities.apply_optional_flags(base_args, requested)
    }

    fn discovery_flags(requested: &OptionalNixFlags) -> OptionalNixFlags {
        let mut flags = requested.clone();
        flags.no_write_lock_file = true;
        flags
    }

    /// Discover flake apps for the adapter's current system.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when flakes are disabled, `nix flake show` fails, or
    /// its JSON cannot be parsed.
    pub fn discover_apps(
        &self,
        flake_ref: &str,
        requested: &OptionalNixFlags,
    ) -> Result<Vec<App>, NixError> {
        self.require_flakes()?;
        let args = self.compatible_argv(
            command::flake_show_args(flake_ref),
            &Self::discovery_flags(requested),
        );
        discovery::discover_apps_with_args(&self.nix, &self.system, flake_ref, &args)
    }

    /// Discover versioned task metadata (`nxr.<system>`) for the current system.
    ///
    /// Missing `nxr` output yields an empty [`TaskDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`TaskDiscoveryError`] when evaluation or schema validation fails.
    pub fn discover_tasks(
        &self,
        flake_ref: &str,
        requested: &OptionalNixFlags,
    ) -> Result<TaskDocument, TaskDiscoveryError> {
        self.require_flakes()?;
        let args = self.compatible_argv(
            command::flake_eval_json_args(flake_ref, &tasks::tasks_attr_path(&self.system)),
            &Self::discovery_flags(requested),
        );
        tasks::discover_tasks_with_args(&self.nix, &self.system, &args)
    }

    /// Discover packages, checks, or development shells via `nix flake show`.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when flakes are disabled, `nix flake show` fails, or
    /// its JSON cannot be parsed.
    pub fn discover_outputs(
        &self,
        flake_ref: &str,
        table: discovery::OutputTable,
        requested: &OptionalNixFlags,
    ) -> Result<Vec<nxr_core::FlakeOutput>, NixError> {
        self.require_flakes()?;
        let args = self.compatible_argv(
            command::flake_show_args(flake_ref),
            &Self::discovery_flags(requested),
        );
        discovery::discover_outputs_with_args(&self.nix, &self.system, flake_ref, table, &args)
    }

    /// Build a capability-aware `nix run` argv for an app.
    ///
    /// # Errors
    ///
    /// Returns [`NixError::FlakesDisabled`] when flakes are not enabled.
    pub fn nix_run_argv(
        &self,
        flake_ref: &str,
        app_name: &str,
        forwarded_args: &[impl AsRef<str>],
        requested: &OptionalNixFlags,
    ) -> Result<Vec<String>, NixError> {
        self.require_flakes()?;
        Ok(self.compatible_argv(
            command::nix_run_args(flake_ref, app_name, forwarded_args),
            requested,
        ))
    }

    /// Capability-aware `nix build` argv for an installable.
    ///
    /// # Errors
    ///
    /// Returns [`NixError::FlakesDisabled`] when flakes are not enabled.
    pub fn nix_build_argv(
        &self,
        installable: &str,
        requested: &OptionalNixFlags,
    ) -> Result<Vec<String>, NixError> {
        self.require_flakes()?;
        Ok(self.compatible_argv(command::nix_build_args(installable), requested))
    }

    /// Capability-aware `nix flake check` argv.
    ///
    /// # Errors
    ///
    /// Returns [`NixError::FlakesDisabled`] when flakes are not enabled.
    pub fn nix_flake_check_argv(
        &self,
        flake_ref: &str,
        requested: &OptionalNixFlags,
    ) -> Result<Vec<String>, NixError> {
        self.require_flakes()?;
        Ok(self.compatible_argv(command::nix_flake_check_args(flake_ref), requested))
    }

    /// Capability-aware interactive `nix develop` argv.
    ///
    /// # Errors
    ///
    /// Returns [`NixError::FlakesDisabled`] when flakes are not enabled.
    pub fn nix_develop_argv(
        &self,
        flake_ref: &str,
        shell_name: Option<&str>,
        requested: &OptionalNixFlags,
    ) -> Result<Vec<String>, NixError> {
        self.require_flakes()?;
        Ok(self.compatible_argv(command::nix_develop_args(flake_ref, shell_name), requested))
    }
}

#[cfg(test)]
mod tests {
    use super::NixAdapter;
    use crate::NixError;
    use crate::capabilities::{NixFailureKind, NixVersion, OptionalNixFlags};
    use camino::Utf8PathBuf;
    use nxr_core::diagnostics::exit;

    #[test]
    #[ignore = "requires nix and fixture flakes"]
    fn discover_apps_from_basic_apps_fixture() {
        let adapter = NixAdapter::new().expect("adapter");
        let apps = adapter
            .discover_apps("./fixtures/basic-apps", &OptionalNixFlags::default())
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
            .discover_tasks(fixture.as_str(), &OptionalNixFlags::default())
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
            .discover_tasks(fixture.as_str(), &OptionalNixFlags::default())
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
    fn flakes_disabled_maps_to_capability_exit_code() {
        let error = NixError::FlakesDisabled {
            version: NixVersion::new(2, 18, 1),
        };
        assert_eq!(error.exit_code(), exit::NIX_CAPABILITY);
        assert!(error.user_message().contains("flakes"));
        assert!(error.user_message().contains("experimental-features"));
    }

    #[test]
    fn require_flakes_errors_when_disabled() {
        let adapter = NixAdapter::with_parts(
            Utf8PathBuf::from("/nix/bin/nix"),
            "aarch64-darwin".to_owned(),
            crate::capabilities::NixCapabilities {
                version: NixVersion::new(2, 18, 1),
                flakes_enabled: false,
                supports_json_log_format: true,
                supports_no_write_lock_file: false,
                supports_offline: true,
                supports_accept_flake_config: false,
            },
        );
        let error = adapter.require_flakes().expect_err("flakes disabled");
        assert!(matches!(error, NixError::FlakesDisabled { .. }));
    }

    #[test]
    fn compatible_argv_injects_only_supported_flags() {
        let adapter = NixAdapter::with_parts(
            Utf8PathBuf::from("/nix/bin/nix"),
            "aarch64-darwin".to_owned(),
            crate::capabilities::NixCapabilities {
                version: NixVersion::new(2, 18, 1),
                flakes_enabled: true,
                supports_json_log_format: false,
                supports_no_write_lock_file: true,
                supports_offline: false,
                supports_accept_flake_config: true,
            },
        );
        let args = adapter.compatible_argv(
            vec!["run".to_owned(), ".#hello".to_owned()],
            &OptionalNixFlags {
                offline: true,
                no_write_lock_file: true,
                accept_flake_config: true,
                json_log_format: true,
                nix_options: Vec::new(),
                extra_argv: Vec::new(),
            },
        );
        assert_eq!(
            args,
            vec![
                "--accept-flake-config".to_owned(),
                "run".to_owned(),
                "--no-write-lock-file".to_owned(),
                ".#hello".to_owned(),
            ]
        );
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
