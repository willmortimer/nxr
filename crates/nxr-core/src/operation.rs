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
    /// Windows build-task id. This variant does not start WSL.
    WinBuildTask { task_id: String },
    /// Placement for a worker reached through Hub. The CLI dials it.
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
    },
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
            Self::FlakeApp { .. } | Self::NixBuild { .. } | Self::WorkspaceScript { .. }
        )
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
            }
            .requires_nix()
        );
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
