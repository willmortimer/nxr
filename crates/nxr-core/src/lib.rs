//! Shared models, schema versions, diagnostics, and policy types.

pub mod config;
pub mod diagnostics;
pub mod env_policy;
pub mod model;
pub mod plan;
pub mod sanitize;

pub use diagnostics::{Diagnostic, DiagnosticLevel};
pub use env_policy::{CLEAN_ENV_ALLOWLIST, EnvironmentPolicy, parse_env_name, parse_set_env};
pub use model::{App, AppList, FlakeRef, ListApp};
pub use plan::{Plan, PlanCommand, PlanKind};
pub use sanitize::sanitize_terminal_text;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::diagnostics::exit;
    use super::env_policy::EnvironmentPolicy;
    use super::model::{App, AppList, ListApp};
    use super::plan::{Plan, PlanCommand, PlanKind};
    use serde_json::json;

    #[test]
    fn app_list_serde_round_trip_matches_contract_shape() {
        let envelope = AppList::new(
            ".",
            "aarch64-darwin",
            vec![ListApp {
                name: "test".to_owned(),
                attr_path: "apps.aarch64-darwin.test".to_owned(),
                description: Some("Run the test suite".to_owned()),
                is_default: false,
            }],
        );

        let value = serde_json::to_value(&envelope).expect("serialize AppList");
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "flake": ".",
                "system": "aarch64-darwin",
                "apps": [
                    {
                        "name": "test",
                        "attr_path": "apps.aarch64-darwin.test",
                        "description": "Run the test suite",
                        "default": false
                    }
                ]
            })
        );

        let restored: AppList =
            serde_json::from_value(value).expect("deserialize AppList into same shape");
        assert_eq!(restored, envelope);
    }

    #[test]
    fn app_list_from_apps_omits_empty_description() {
        let envelope = AppList::from_apps(
            ".",
            "aarch64-darwin",
            [App {
                name: "lint".to_owned(),
                attr_path: "apps.aarch64-darwin.lint".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: true,
                metadata: BTreeMap::default(),
            }],
        );

        let value = serde_json::to_value(&envelope).expect("serialize AppList");
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "flake": ".",
                "system": "aarch64-darwin",
                "apps": [
                    {
                        "name": "lint",
                        "attr_path": "apps.aarch64-darwin.lint",
                        "default": true
                    }
                ]
            })
        );
    }

    #[test]
    fn plan_serde_round_trip_matches_contract_shape() {
        let flake = "/absolute/project/path";
        let invocation = "/absolute/project/path/crates/api";
        let envelope = Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: flake.to_owned(),
            system: "aarch64-darwin".to_owned(),
            target: "test".to_owned(),
            attr_path: "apps.aarch64-darwin.test".to_owned(),
            invocation_directory: invocation.to_owned(),
            execution_directory: invocation.to_owned(),
            environment_policy: EnvironmentPolicy::Inherit,
            command: PlanCommand {
                program: "nix".to_owned(),
                arguments: vec!["run".to_owned(), format!("{flake}#test"), "--".to_owned()],
            },
            forwarded_arguments: vec![],
        };

        let value = serde_json::to_value(&envelope).expect("serialize Plan");
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "kind": "app",
                "flake": flake,
                "system": "aarch64-darwin",
                "target": "test",
                "attr_path": "apps.aarch64-darwin.test",
                "invocation_directory": invocation,
                "execution_directory": invocation,
                "environment_policy": "inherit",
                "command": {
                    "program": "nix",
                    "arguments": [
                        "run",
                        "/absolute/project/path#test",
                        "--"
                    ]
                },
                "forwarded_arguments": []
            })
        );

        let restored: Plan = serde_json::from_value(value).expect("deserialize Plan");
        assert_eq!(restored, envelope);
    }

    #[test]
    fn plan_serde_round_trip_preserves_forwarded_arguments() {
        let envelope = Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: ".".to_owned(),
            system: "x86_64-linux".to_owned(),
            target: "lint".to_owned(),
            attr_path: "apps.x86_64-linux.lint".to_owned(),
            invocation_directory: "/work".to_owned(),
            execution_directory: "/work".to_owned(),
            environment_policy: EnvironmentPolicy::Inherit,
            command: PlanCommand {
                program: "nix".to_owned(),
                arguments: vec!["run".to_owned(), ".#lint".to_owned(), "--".to_owned()],
            },
            forwarded_arguments: vec!["--fix".to_owned(), "--".to_owned(), "extra".to_owned()],
        };

        let value = serde_json::to_value(&envelope).expect("serialize Plan");
        let restored: Plan = serde_json::from_value(value).expect("deserialize Plan");
        assert_eq!(restored, envelope);
    }

    #[test]
    fn exit_code_constants_match_cli_contract() {
        assert_eq!(exit::SUCCESS, 0);
        assert_eq!(exit::CHILD_FAILED, 1);
        assert_eq!(exit::USAGE, 2);
        assert_eq!(exit::DISCOVERY, 3);
        assert_eq!(exit::NIX_CAPABILITY, 4);
        assert_eq!(exit::EVALUATION, 5);
        assert_eq!(exit::NOT_FOUND, 6);
        assert_eq!(exit::INVALID_METADATA, 7);
        assert_eq!(exit::TASK_GRAPH, 8);
        assert_eq!(exit::PROCESS_SUPERVISION, 9);
        assert_eq!(exit::INTERRUPTED, 10);
    }
}
