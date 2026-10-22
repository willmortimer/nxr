//! Provider-neutral CI identity for one workflow run.
//!
//! `ci-run-metadata-v2` keeps `operation_id` null when the compiler did not
//! produce one. A submission key is never stored in that field.

use serde::{Deserialize, Serialize};

use crate::execution_spec::{CiIdentity, ExecutionSpecV2};

pub const CI_RUN_METADATA_V2: &str = "ci-run-metadata-v2";

/// One CI submission. Secret values are not fields of this record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CiRunMetadata {
    pub schema_version: String,
    pub provider: String,
    pub external_job_id: String,
    pub run_id: String,
    pub job: String,
    pub run_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub exit_code: i32,
}

impl CiRunMetadata {
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        run_id: impl Into<String>,
        job: impl Into<String>,
        run_attempt: u32,
        run_url: Option<String>,
        operation_id: Option<String>,
        exit_code: i32,
    ) -> Self {
        let run_id = run_id.into();
        let job = job.into();
        let provider = provider.into();
        Self {
            schema_version: CI_RUN_METADATA_V2.to_owned(),
            external_job_id: format!("{run_id}/{job}/{run_attempt}"),
            idempotency_key: format!("{provider}:{run_id}/{job}/{run_attempt}"),
            provider,
            run_id,
            job,
            run_attempt,
            run_url,
            operation_id,
            exit_code,
        }
    }
}

/// Copy CI identity onto a spec. The leaf operation id stays put.
pub fn attach_ci_run(spec: &mut ExecutionSpecV2, metadata: &CiRunMetadata) {
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
    use crate::identity::{SourceSetV1, workflow_definition_digest};
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
        let spec = execution_spec_for_task(
            &SourceSetV1::fixture("src_fixture"),
            &workflow_definition_digest(b"finish"),
            "finish",
            &task,
        )
        .expect("spec");
        let dir = tempfile::tempdir().expect("temp");
        let ran = execute_spec_locally(&spec, dir.path(), "run_ci").expect("local");
        assert_eq!(ran.exit_code, 0);

        let metadata = CiRunMetadata::new(
            "github",
            "4242",
            "build",
            1,
            Some("https://github.com/willmortimer/nxr/actions/runs/4242".to_owned()),
            Some(ran.operation_id.clone()),
            ran.exit_code,
        );
        assert_eq!(metadata.external_job_id, "4242/build/1");
        let mut attached = spec.clone();
        attach_ci_run(&mut attached, &metadata);
        assert_eq!(attached.operation_id, spec.operation_id);
        assert_eq!(attached.idempotency_key, "github:4242/build/1");
        let rendered = serde_json::to_string(&attached.redacted()).expect("spec");
        assert!(!rendered.contains("do-not-publish"));

        let out = dir.path().join("ci-run-metadata.json");
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/actions/nxr-workflow/write-metadata.sh");
        let status = Command::new(&script)
            .arg(&out)
            .arg(&metadata.provider)
            .arg(&metadata.run_id)
            .arg(&metadata.job)
            .arg(metadata.run_attempt.to_string())
            .arg(metadata.run_url.as_deref().unwrap_or(""))
            .arg(metadata.operation_id.as_deref().unwrap_or(""))
            .arg(metadata.exit_code.to_string())
            .status()
            .expect("writer");
        assert!(status.success());
        let written: CiRunMetadata =
            serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("parse");
        assert_eq!(written, metadata);
        let unknown = dir.path().join("unknown.json");
        let status = Command::new(&script)
            .arg(&unknown)
            .arg("github")
            .arg("4242")
            .arg("build")
            .arg("2")
            .arg("")
            .arg("")
            .arg("1")
            .status()
            .expect("writer");
        assert!(status.success());
        let missing: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&unknown).expect("read")).expect("parse");
        assert!(missing.get("operation_id").expect("key").is_null());
        assert_eq!(missing["external_job_id"], "4242/build/2");
        assert_eq!(missing["exit_code"], 1);
    }
}
