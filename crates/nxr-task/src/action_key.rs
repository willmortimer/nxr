//! Workspace action key material for local CAS ([ADR-0147]).

use std::collections::BTreeMap;

use camino::Utf8Path;
use nxr_core::cas::{CAS_PROTOCOL_VERSION, flake_lock_digest, hash_action_key};
use nxr_core::{ActionTier, classify_action_tier, workspace_cache_enabled};
use serde::Serialize;

use crate::schema::{EnvInput, TaskCacheMode, TaskDefinition, TaskDocument};

/// Resolved cache plan for one task node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceCachePlan {
    pub tier: ActionTier,
    pub cache_enabled: bool,
    pub restore: bool,
    pub save: bool,
    pub action_key: Option<String>,
    pub output_paths: Vec<String>,
    pub key_components: BTreeMap<String, String>,
}

/// Build cache metadata for a task before execution.
///
/// # Errors
///
/// Returns [`std::io::Error`] when path or lockfile digests cannot be computed.
pub fn build_workspace_cache_plan(
    document: &TaskDocument,
    task_id: &str,
    definition: &TaskDefinition,
    system: &str,
    flake_root: &Utf8Path,
    cwd: &str,
    context_name: Option<&str>,
    upstream_keys: &BTreeMap<String, String>,
) -> std::io::Result<WorkspaceCachePlan> {
    let cache_mode = definition
        .cache
        .as_ref()
        .and_then(|cache| cache.mode.as_ref());
    let mode_label = cache_mode_label(cache_mode);
    let tier = classify_action_tier(definition.outputs.len());
    let cache_enabled = workspace_cache_enabled(definition.outputs.len(), mode_label);
    let output_paths: Vec<String> = definition.outputs.iter().map(|o| o.path.clone()).collect();

    let restore = definition
        .cache
        .as_ref()
        .and_then(|c| c.restore)
        .unwrap_or(true);
    let save = definition
        .cache
        .as_ref()
        .and_then(|c| c.save)
        .unwrap_or(true);

    if !cache_enabled {
        return Ok(WorkspaceCachePlan {
            tier,
            cache_enabled: false,
            restore: false,
            save: false,
            action_key: None,
            output_paths,
            key_components: BTreeMap::new(),
        });
    }

    let mut key_components = BTreeMap::new();
    key_components.insert(
        "protocol_version".to_owned(),
        CAS_PROTOCOL_VERSION.to_string(),
    );
    key_components.insert(
        "schema_major".to_owned(),
        document.schema_version.to_string(),
    );
    key_components.insert("task_id".to_owned(), task_id.to_owned());
    if let Some(salt) = definition.cache.as_ref().and_then(|c| c.version.as_ref()) {
        key_components.insert("cache_salt".to_owned(), salt.clone());
    }
    key_components.insert("system".to_owned(), system.to_owned());
    if let Some(digest) = flake_lock_digest(flake_root)? {
        key_components.insert("flake_lock_digest".to_owned(), digest);
    }
    key_components.insert("app".to_owned(), definition.app.clone());
    key_components.insert("cwd".to_owned(), cwd.to_owned());
    if let Some(context) = context_name {
        key_components.insert("context".to_owned(), context.to_owned());
    }

    if let Some(inputs) = &definition.inputs {
        for path in &inputs.paths {
            let digest = nxr_core::cas::digest_repo_path(flake_root, path)?;
            key_components.insert(format!("input.path:{path}"), digest);
        }
        for env_input in &inputs.env {
            let (name, secret) = env_input_name_secret(env_input);
            if secret {
                continue;
            }
            let value = std::env::var(&name).unwrap_or_default();
            key_components.insert(format!("input.env:{name}"), value);
        }
        for (binding, upstream) in &inputs.bindings {
            let digest = upstream_keys
                .get(&upstream.from)
                .cloned()
                .unwrap_or_else(|| "pending".to_owned());
            key_components.insert(format!("input.binding:{binding}"), digest);
        }
    }

    let material = serde_json::to_value(&KeyMaterial {
        protocol_version: CAS_PROTOCOL_VERSION,
        schema_major: document.schema_version,
        task_id: task_id.to_owned(),
        cache_salt: definition.cache.as_ref().and_then(|c| c.version.clone()),
        system: system.to_owned(),
        flake_lock_digest: key_components.get("flake_lock_digest").cloned(),
        app: definition.app.clone(),
        cwd: cwd.to_owned(),
        context: context_name.map(str::to_owned),
        inputs: key_components
            .iter()
            .filter(|(k, _)| k.starts_with("input."))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    })
    .unwrap_or(serde_json::Value::Null);

    let action_key = hash_action_key(&material);

    Ok(WorkspaceCachePlan {
        tier,
        cache_enabled: true,
        restore,
        save,
        action_key: Some(action_key),
        output_paths,
        key_components,
    })
}

fn cache_mode_label(mode: Option<&TaskCacheMode>) -> Option<&str> {
    mode.map(|mode| match mode {
        TaskCacheMode::Disabled => "disabled",
        TaskCacheMode::Local => "local",
        TaskCacheMode::SharedRead => "shared-read",
        TaskCacheMode::Shared => "shared",
    })
}

fn env_input_name_secret(input: &EnvInput) -> (String, bool) {
    match input {
        EnvInput::Name(name) => (name.clone(), false),
        EnvInput::Binding(binding) => (binding.name.clone(), binding.secret),
    }
}

#[derive(Serialize)]
struct KeyMaterial {
    protocol_version: u32,
    schema_major: u32,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_salt: Option<String>,
    system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flake_lock_digest: Option<String>,
    app: String,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    inputs: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{TaskCache, TaskCacheMode, TaskDefinition, TaskDocument, TaskOutput};

    fn workspace_task() -> TaskDefinition {
        let mut def = TaskDefinition::new("codegen");
        def.outputs = vec![TaskOutput {
            path: "gen/out".to_owned(),
            mode: None,
            optional: false,
        }];
        def.cache = Some(TaskCache {
            mode: Some(TaskCacheMode::Local),
            version: None,
            restore: None,
            save: None,
            failures: None,
        });
        def
    }

    #[test]
    fn derivation_task_has_no_action_key() {
        let doc = TaskDocument {
            schema_version: 2,
            tasks: BTreeMap::new(),
            apps: BTreeMap::new(),
            discovery_inputs: Vec::new(),
            contexts: BTreeMap::new(),
            processes: BTreeMap::new(),
        };
        let def = TaskDefinition::new("test");
        let plan = build_workspace_cache_plan(
            &doc,
            "test",
            &def,
            "aarch64-darwin",
            camino::Utf8Path::new("/tmp"),
            "/tmp",
            None,
            &BTreeMap::new(),
        )
        .expect("plan");
        assert_eq!(plan.tier, ActionTier::DerivationBacked);
        assert!(!plan.cache_enabled);
        assert!(plan.action_key.is_none());
    }

    #[test]
    fn workspace_task_computes_stable_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = TaskDocument {
            schema_version: 2,
            tasks: BTreeMap::new(),
            apps: BTreeMap::new(),
            discovery_inputs: Vec::new(),
            contexts: BTreeMap::new(),
            processes: BTreeMap::new(),
        };
        let def = workspace_task();
        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            "/tmp/proj",
            None,
            &BTreeMap::new(),
        )
        .expect("plan");
        assert_eq!(plan.tier, ActionTier::WorkspaceAction);
        assert!(plan.cache_enabled);
        assert!(plan.action_key.is_some());
    }
}
