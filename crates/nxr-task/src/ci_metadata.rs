//! Provider-neutral CI identity for one workflow run.
//!
//! The JSON keys are the metadata file written by
//! `.github/actions/nxr-workflow`. NXR does not import NixPlane. Callers copy
//! `provider`, `external_job_id`, and `run_url` onto [`ExecutionSpecV1::ci`].

use serde::{Deserialize, Serialize};

use crate::execution_spec::{CiIdentity, ExecutionSpecV1};

pub const CI_RUN_METADATA_V1: &str = "ci-run-metadata-v1";

/// One CI submission. Secret values are not fields of this record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CiRunMetadata {
    pub schema_version: String,
    pub provider: String,
    pub external_job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    pub idempotency_key: String,
    pub operation_id: String,
    pub exit_code: i32,
}

impl CiRunMetadata {
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        external_job_id: impl Into<String>,
        run_url: Option<String>,
        idempotency_key: impl Into<String>,
        operation_id: impl Into<String>,
        exit_code: i32,
    ) -> Self {
        Self {
            schema_version: CI_RUN_METADATA_V1.to_owned(),
            provider: provider.into(),
            external_job_id: external_job_id.into(),
            run_url,
            idempotency_key: idempotency_key.into(),
            operation_id: operation_id.into(),
            exit_code,
        }
    }
}

/// Copy CI identity onto a spec. The leaf operation id stays put.
pub fn attach_ci_run(spec: &mut ExecutionSpecV1, metadata: &CiRunMetadata) {
    spec.ci = Some(CiIdentity {
        provider: metadata.provider.clone(),
        external_job_id: metadata.external_job_id.clone(),
        run_url: metadata.run_url.clone(),
    });
    spec.idempotency_key = metadata.idempotency_key.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;

    use nxr_core::Operation;

    use crate::execution_spec::{execute_spec_locally, execution_spec_for_task};
    use crate::resources::NodeResources;
    use crate::workflows::{WorkflowPlatform, WorkflowTask};

    #[test]
    fn local_run_and_action_writer_share_metadata_shape() {
        let task = WorkflowTask {
            depends_on: Vec::new(),
            platform: WorkflowPlatform::Unix,
            architecture: None,
            resources: NodeResources::EMPTY,
            environment: BTreeMap::new(),
            working_directory: None,
            artifacts: Vec::new(),
            operation: Operation::NativeCommand {
                argv: vec!["/usr/bin/true".to_owned()],
            },
        };
        let spec = execution_spec_for_task("src_fixture", "finish", &task);
        let dir = tempfile::tempdir().expect("temp");
        let ran =
            execute_spec_locally(&spec, &dir.path().join("events.jsonl"), "run_ci").expect("local");
        assert_eq!(ran.exit_code, 0);

        let metadata = CiRunMetadata::new(
            "github",
            "4242",
            Some("https://github.com/willmortimer/nxr/actions/runs/4242".to_owned()),
            "github:4242:finish",
            ran.operation_id.clone(),
            ran.exit_code,
        );
        let mut attached = spec.clone();
        attach_ci_run(&mut attached, &metadata);
        assert_eq!(attached.operation_id, spec.operation_id);
        assert_eq!(attached.idempotency_key, "github:4242:finish");
        assert_eq!(
            attached.ci.as_ref().map(|ci| ci.provider.as_str()),
            Some("github")
        );
        let rendered = serde_json::to_string(&attached).expect("spec");
        assert!(!rendered.contains("do-not-publish"));

        let out = dir.path().join("ci-run-metadata.json");
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/actions/nxr-workflow/write-metadata.sh");
        let status = Command::new(&script)
            .arg(&out)
            .arg(&metadata.provider)
            .arg(&metadata.external_job_id)
            .arg(metadata.run_url.as_deref().unwrap_or(""))
            .arg(&metadata.idempotency_key)
            .arg(&metadata.operation_id)
            .arg(metadata.exit_code.to_string())
            .status()
            .expect("writer");
        assert!(status.success());
        let written: CiRunMetadata =
            serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("parse");
        assert_eq!(written, metadata);
        let value = serde_json::to_value(&written).expect("value");
        for key in [
            "schema_version",
            "provider",
            "external_job_id",
            "run_url",
            "idempotency_key",
            "operation_id",
            "exit_code",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
    }
}
