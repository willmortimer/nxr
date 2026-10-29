//! Internal operation IR.
//!
//! Every graph node carries one [`Operation`]. Existing task documents still
//! require `app`; planning desugars that string into [`Operation::FlakeApp`].
//! This is not a schema major and not a project file.

use serde::{Deserialize, Serialize};

/// One executable step in an NXR graph.
///
/// Workflow files may only name a flake app, a native argv, or a winbuild task
/// id. The other variants exist so later surfaces can desugar into the same
/// node type without a second graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    /// Flake app leaf (`apps.<system>.<name>`). Realization stays `nix run`.
    FlakeApp { name: String },
    /// Nix installable (`nix build`). Realization stays the Nix CLI.
    NixBuild { installable: String },
    /// Direct argv. No shell evaluation and no Nix lookup.
    NativeCommand { argv: Vec<String> },
    /// Checkout script reference. The current script command still locates Nix.
    WorkspaceScript {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interpreter: Option<String>,
    },
    /// Supervised long-lived process (desugared from process tasks).
    Process {
        app: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        readiness: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restart: Option<ProcessRestartPolicy>,
    },
    /// WASI component leaf (V4.5 scaffolding; not executed yet).
    WasiComponent {
        component: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        world: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    /// External adapter hook (Hub inspector, deploy guard, …).
    Adapter {
        adapter_kind: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        configuration: serde_json::Value,
    },
    /// Windows build-task id. This variant does not start WSL.
    ///
    /// `manifest` is relative to the selected workspace and defaults to
    /// `.winbuild.json`.
    WinBuildTask {
        task_id: String,
        #[serde(default = "default_winbuild_manifest")]
        manifest: String,
    },
    /// Placement for a worker reached through Hub. The CLI dials it.
    ///
    /// A development task may leave `source_revision` and `lock_digest` empty
    /// until the coordinator resolves `source_ref` and `lock_file`. The worker
    /// receives only the resolved commit and digest.
    Remote {
        target: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        task: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        source_url: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        source_revision: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        lock_digest: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        source_ref: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        lock_file: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        build_root: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        manifest: String,
    },
}

/// Restart policy for [`Operation::Process`] (V4 scaffolding).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRestartPolicy {
    Never,
    OnFailure,
    Always,
}

fn default_winbuild_manifest() -> String {
    ".winbuild.json".to_owned()
}

impl Operation {
    /// Desugar a required task `app` string into a flake-app operation.
    #[must_use]
    pub fn from_task_app(app: impl Into<String>) -> Self {
        Self::FlakeApp { name: app.into() }
    }

    /// Stable kind label (`flake_app`, `nix_build`, …).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::FlakeApp { .. } => "flake_app",
            Self::NixBuild { .. } => "nix_build",
            Self::NativeCommand { .. } => "native_command",
            Self::WorkspaceScript { .. } => "workspace_script",
            Self::Process { .. } => "process",
            Self::WasiComponent { .. } => "wasi_component",
            Self::Adapter { .. } => "adapter",
            Self::WinBuildTask { .. } => "win_build_task",
            Self::Remote { .. } => "remote",
        }
    }

    /// Whether running this operation needs a Nix executable.
    ///
    /// Native commands and winbuild tasks are false. Workspace scripts are true
    /// because `nxr script` locates a Nix binary today.
    #[must_use]
    pub const fn requires_nix(&self) -> bool {
        matches!(
            self,
            Self::FlakeApp { .. }
                | Self::NixBuild { .. }
                | Self::WorkspaceScript { .. }
                | Self::Process { .. }
        )
    }

    /// Desugar a process task app name into a supervised process operation.
    #[must_use]
    pub fn from_process_app(app: impl Into<String>) -> Self {
        Self::Process {
            app: app.into(),
            readiness: String::new(),
            restart: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_app_desugars_to_flake_app() {
        let operation = Operation::from_task_app("ci");
        assert_eq!(
            operation,
            Operation::FlakeApp {
                name: "ci".to_owned()
            }
        );
        assert!(operation.requires_nix());
        assert_eq!(operation.kind(), "flake_app");
    }

    #[test]
    fn native_and_winbuild_do_not_require_nix() {
        assert!(
            !Operation::NativeCommand {
                argv: vec!["/bin/echo".to_owned()],
            }
            .requires_nix()
        );
        assert!(
            !Operation::WinBuildTask {
                task_id: "link".to_owned(),
                manifest: ".winbuild.json".to_owned(),
            }
            .requires_nix()
        );
        assert!(
            !Operation::Remote {
                target: "builder".to_owned(),
                task: String::new(),
                source_url: String::new(),
                source_revision: String::new(),
                lock_digest: String::new(),
                source_ref: String::new(),
                lock_file: String::new(),
                build_root: String::new(),
                manifest: String::new(),
            }
            .requires_nix()
        );
    }

    #[test]
    fn process_and_adapter_round_trip_in_operation_ir_envelope() {
        use crate::operation_ir::{OperationIrEnvelope, OPERATION_IR_SCHEMA_VERSION};

        let operation = Operation::Process {
            app: "api".to_owned(),
            readiness: "http://127.0.0.1:8080/health".to_owned(),
            restart: Some(ProcessRestartPolicy::OnFailure),
        };
        let envelope = OperationIrEnvelope::new(&operation);
        assert_eq!(envelope.schema_version, OPERATION_IR_SCHEMA_VERSION);
        assert_eq!(envelope.operation, operation);
    }

    #[test]
    fn operation_json_round_trip() {
        let operation = Operation::NativeCommand {
            argv: vec!["/bin/echo".to_owned(), "ok".to_owned()],
        };
        let value = serde_json::to_value(&operation).expect("serialize");
        assert_eq!(value["kind"], "native_command");
        let restored: Operation = serde_json::from_value(value).expect("deserialize");
        assert_eq!(restored, operation);
    }
}
