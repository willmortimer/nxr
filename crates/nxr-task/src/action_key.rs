//! Workspace action key material for local CAS ([ADR-0147]).
//!
//! Key material is a canonical JSON structure hashed with BLAKE3. Secret values
//! never appear in the material; unresolved upstream bindings disable caching.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use globset::Glob;
use nxr_core::cas::{
    CAS_PROTOCOL_VERSION, CasOutput, CasRestoreMode, digest_bytes, digest_file, digest_repo_path,
    flake_lock_digest, hash_action_key,
};
use nxr_core::{
    ActionTier, EnvironmentPolicy, cache_mode_shared_unimplemented, classify_action_tier,
    workspace_cache_enabled,
};
use nxr_core::{normalize_repo_relative_path, validate_repo_relative_path};
use serde::Serialize;

use crate::context::PlanSecretEntry;
use crate::schema::{
    EnvInput, TaskCacheMode, TaskCacheSecretPolicy, TaskDefinition, TaskDocument, TaskOutput,
    TaskOutputMode,
};

/// Resolved cache plan for one task node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceCachePlan {
    pub tier: ActionTier,
    pub cache_enabled: bool,
    pub restore: bool,
    pub save: bool,
    pub action_key: Option<String>,
    pub outputs: Vec<CasOutput>,
    pub key_components: BTreeMap<String, String>,
}

/// Execution-time inputs that affect workspace cache key material.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceCachePlanOptions {
    /// Root-task forwarded arguments (after `--` stripping policy).
    pub forwarded_args: Vec<String>,
    /// Prepared child program when available.
    pub command_program: Option<String>,
    /// Prepared argv when available.
    pub command_argv: Vec<String>,
    /// Effective devShell name for this node.
    pub effective_shell: Option<String>,
    /// Spawn environment policy for this node.
    pub environment_policy: Option<EnvironmentPolicy>,
    /// Named execution context.
    pub context_name: Option<String>,
    /// Secret slot metadata (refs only; no values).
    pub context_secrets: Vec<PlanSecretEntry>,
    /// Non-secret context `environment.set` entries applied at spawn.
    pub context_spawn_env_set: BTreeMap<String, String>,
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
    execution_cwd: &str,
    upstream_keys: &BTreeMap<String, String>,
    options: &WorkspaceCachePlanOptions,
) -> io::Result<WorkspaceCachePlan> {
    let cache_mode = definition
        .cache
        .as_ref()
        .and_then(|cache| cache.mode.as_ref());
    let mode_label = cache_mode_label(cache_mode);
    if cache_mode_shared_unimplemented(mode_label) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cache.mode `{label}` is not implemented yet; use `local` or `disabled` until a shared CAS transport exists (nxr#2)",
                label = mode_label.unwrap_or("shared")
            ),
        ));
    }
    let tier = classify_action_tier(definition.outputs.len());
    let cache_requested = workspace_cache_enabled(definition.outputs.len(), mode_label);
    let outputs = cas_outputs_from_task(&definition.outputs);

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

    if !cache_requested {
        return Ok(disabled_plan(
            tier,
            outputs,
            BTreeMap::from([(
                "cache_disabled".to_owned(),
                "no workspace outputs or cache mode disabled".to_owned(),
            )]),
        ));
    }

    let has_secret_env = definition.inputs.as_ref().is_some_and(|inputs| {
        inputs
            .env
            .iter()
            .any(|env_input| env_input_metadata(env_input).2)
    });
    let has_context_secrets = !options.context_secrets.is_empty();
    let ignore_secret_values = matches!(
        definition
            .cache
            .as_ref()
            .and_then(|cache| cache.secret_policy),
        Some(TaskCacheSecretPolicy::IgnoreValues)
    );
    if (has_secret_env || has_context_secrets) && !ignore_secret_values {
        return Ok(disabled_plan(
            tier,
            outputs,
            BTreeMap::from([(
                "cache_disabled".to_owned(),
                "secret-bearing task disables workspace cache by default".to_owned(),
            )]),
        ));
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

    let task_definition_digest = canonical_task_definition_digest(definition)?;
    key_components.insert("task_definition".to_owned(), task_definition_digest.clone());

    if let Some(salt) = definition.cache.as_ref().and_then(|c| c.version.as_ref()) {
        key_components.insert("cache_salt".to_owned(), salt.clone());
    }
    key_components.insert("system".to_owned(), system.to_owned());

    if let Some(digest) = flake_lock_digest(flake_root)? {
        key_components.insert("flake_lock_digest".to_owned(), digest);
    }
    if let Some(digest) = flake_nix_digest(flake_root)? {
        key_components.insert("flake_nix_digest".to_owned(), digest);
    }

    for (index, input) in document.discovery_inputs.iter().enumerate() {
        let digest = digest_repo_path(flake_root, input)?;
        key_components.insert(format!("discovery_input[{index}]"), digest);
    }

    let relative_cwd = relative_cwd_for_key(flake_root, execution_cwd)?;
    key_components.insert("cwd".to_owned(), relative_cwd.clone());

    if let Some(shell) = options
        .effective_shell
        .as_deref()
        .or(definition.shell.as_deref())
    {
        key_components.insert("shell".to_owned(), shell.to_owned());
    }

    if let Some(context) = options.context_name.as_deref() {
        key_components.insert("context".to_owned(), context.to_owned());
    }

    if let Some(policy) = &options.environment_policy {
        let policy_json = serde_json::to_string(policy).unwrap_or_default();
        key_components.insert(
            "environment_policy".to_owned(),
            digest_bytes(policy_json.as_bytes()),
        );
    }

    for secret in &options.context_secrets {
        // Never include resolved secret values in explainable key material.
        key_components.insert(
            format!("context.secret:{}", secret.name),
            format!(
                "{}:{}:{:?}",
                secret.reference,
                secret_provider_label(secret.provider),
                secret.delivery,
            ),
        );
    }
    for (name, value) in &options.context_spawn_env_set {
        key_components.insert(
            format!("context.env_set:{name}"),
            digest_bytes(value.as_bytes()),
        );
    }

    if let Some(program) = &options.command_program {
        key_components.insert("command.program".to_owned(), program.clone());
    }
    if !options.command_argv.is_empty() {
        let argv_json = serde_json::to_string(&options.command_argv).unwrap_or_default();
        key_components.insert(
            "command.argv".to_owned(),
            digest_bytes(argv_json.as_bytes()),
        );
    }
    if !options.forwarded_args.is_empty() {
        let args_json = serde_json::to_string(&options.forwarded_args).unwrap_or_default();
        key_components.insert(
            "forwarded_args".to_owned(),
            digest_bytes(args_json.as_bytes()),
        );
    }

    let outputs_material = output_contract_material(definition.outputs.as_slice());
    let outputs_json = serde_json::to_string(&outputs_material).unwrap_or_default();
    key_components.insert("outputs".to_owned(), digest_bytes(outputs_json.as_bytes()));

    let mut path_digests = BTreeMap::new();
    let mut env_states = BTreeMap::new();
    let mut binding_digests = BTreeMap::new();
    let mut include_git_state = false;
    let mut unresolved_upstream = false;
    let mut required_env_missing = false;

    if let Some(inputs) = &definition.inputs {
        include_git_state = inputs.include_git_state;
        path_digests = expand_path_input_digests(flake_root, &inputs.paths)?;
        for env_input in &inputs.env {
            let (name, required, secret) = env_input_metadata(env_input);
            if secret {
                env_states.insert(name.clone(), EnvInputState::Secret);
                key_components.insert(format!("input.env:{name}"), "secret".to_owned());
                continue;
            }
            let state = env_input_state(&name)?;
            if matches!(state, EnvInputState::Unset) && required {
                required_env_missing = true;
                key_components.insert(format!("input.env:{name}"), "required-missing".to_owned());
            } else {
                let label = env_state_label(&state);
                key_components.insert(format!("input.env:{name}"), label.clone());
                env_states.insert(name, state);
            }
        }
        for (binding, upstream) in &inputs.bindings {
            match upstream_keys.get(&upstream.from) {
                Some(digest) => {
                    binding_digests.insert(binding.clone(), digest.clone());
                    key_components.insert(format!("input.binding:{binding}"), digest.clone());
                }
                None => {
                    unresolved_upstream = true;
                    key_components
                        .insert(format!("input.binding:{binding}"), "unresolved".to_owned());
                }
            }
        }
    }

    if unresolved_upstream {
        key_components.insert(
            "cache_disabled".to_owned(),
            "unresolved upstream binding".to_owned(),
        );
        return Ok(disabled_plan(tier, outputs, key_components));
    }
    if required_env_missing {
        key_components.insert(
            "cache_disabled".to_owned(),
            "required environment input missing".to_owned(),
        );
        return Ok(disabled_plan(tier, outputs, key_components));
    }

    let git_state_digest = if include_git_state {
        match compute_git_state_digest(flake_root)? {
            Some(digest) => {
                key_components.insert("git_state".to_owned(), digest.clone());
                Some(digest)
            }
            None => {
                key_components.insert(
                    "cache_disabled".to_owned(),
                    "includeGitState requested but git state unavailable".to_owned(),
                );
                return Ok(disabled_plan(tier, outputs, key_components));
            }
        }
    } else {
        None
    };

    let material = KeyMaterial {
        protocol_version: CAS_PROTOCOL_VERSION,
        schema_major: document.schema_version,
        task_id: task_id.to_owned(),
        task_definition: task_definition_digest,
        cache_salt: definition.cache.as_ref().and_then(|c| c.version.clone()),
        system: system.to_owned(),
        flake_lock_digest: key_components.get("flake_lock_digest").cloned(),
        flake_nix_digest: key_components.get("flake_nix_digest").cloned(),
        discovery_inputs: document
            .discovery_inputs
            .iter()
            .enumerate()
            .filter_map(|(index, _)| {
                key_components
                    .get(&format!("discovery_input[{index}]"))
                    .cloned()
            })
            .collect(),
        app: definition.app.clone(),
        cwd: relative_cwd,
        shell: options
            .effective_shell
            .clone()
            .or_else(|| definition.shell.clone()),
        context: options.context_name.clone(),
        environment_policy: options.environment_policy.clone(),
        context_secrets: options
            .context_secrets
            .iter()
            .map(ContextSecretMaterial::from)
            .collect(),
        context_spawn_env_set: options
            .context_spawn_env_set
            .iter()
            .map(|(name, value)| (name.clone(), digest_bytes(value.as_bytes())))
            .collect(),
        command: command_material(options),
        forwarded_args: options.forwarded_args.clone(),
        path_inputs: path_digests,
        env_inputs: env_states,
        binding_inputs: binding_digests,
        git_state_digest,
        outputs: outputs_material,
    };

    let action_key =
        hash_action_key(&serde_json::to_value(&material).unwrap_or(serde_json::Value::Null));

    Ok(WorkspaceCachePlan {
        tier,
        cache_enabled: true,
        restore,
        save,
        action_key: Some(action_key),
        outputs,
        key_components,
    })
}

fn disabled_plan(
    tier: ActionTier,
    outputs: Vec<CasOutput>,
    key_components: BTreeMap<String, String>,
) -> WorkspaceCachePlan {
    WorkspaceCachePlan {
        tier,
        cache_enabled: false,
        restore: false,
        save: false,
        action_key: None,
        outputs,
        key_components,
    }
}

fn cas_outputs_from_task(outputs: &[TaskOutput]) -> Vec<CasOutput> {
    outputs
        .iter()
        .map(|output| CasOutput {
            path: output.path.clone(),
            mode: output
                .mode
                .as_ref()
                .map(|mode| match mode {
                    TaskOutputMode::Replace => CasRestoreMode::Replace,
                    TaskOutputMode::Merge => CasRestoreMode::Merge,
                    TaskOutputMode::VerifyOnly => CasRestoreMode::VerifyOnly,
                    TaskOutputMode::Report => CasRestoreMode::Report,
                })
                .unwrap_or_default(),
            optional: output.optional,
        })
        .collect()
}

fn cache_mode_label(mode: Option<&TaskCacheMode>) -> Option<&str> {
    mode.map(|mode| match mode {
        TaskCacheMode::Disabled => "disabled",
        TaskCacheMode::Local => "local",
        TaskCacheMode::SharedRead => "shared-read",
        TaskCacheMode::Shared => "shared",
    })
}

fn canonical_task_definition_digest(definition: &TaskDefinition) -> io::Result<String> {
    let json = serde_json::to_string(definition).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("task definition serialization failed: {error}"),
        )
    })?;
    Ok(digest_bytes(json.as_bytes()))
}

fn flake_nix_digest(flake_root: &Utf8Path) -> io::Result<Option<String>> {
    let flake_nix = flake_root.join("flake.nix");
    if !flake_nix.is_file() {
        return Ok(None);
    }
    Ok(Some(digest_file(flake_nix.as_std_path())?))
}

fn relative_cwd_for_key(flake_root: &Utf8Path, execution_cwd: &str) -> io::Result<String> {
    let cwd_path = Utf8Path::new(execution_cwd);
    if let Ok(relative) = cwd_path.strip_prefix(flake_root) {
        return Ok(normalize_relative_cwd(relative.as_str()));
    }

    let canonical_flake = flake_root
        .canonicalize_utf8()
        .unwrap_or_else(|_| flake_root.to_path_buf());
    let canonical_cwd = cwd_path
        .canonicalize_utf8()
        .unwrap_or_else(|_| cwd_path.to_path_buf());
    if let Ok(relative) = canonical_cwd.strip_prefix(&canonical_flake) {
        return Ok(normalize_relative_cwd(relative.as_str()));
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "execution cwd `{execution_cwd}` is not under flake root `{}`",
            flake_root.as_str()
        ),
    ))
}

fn normalize_relative_cwd(relative: &str) -> String {
    let trimmed = relative.trim_start_matches('/');
    if trimmed.is_empty() {
        ".".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn looks_like_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

fn expand_path_input_digests(
    flake_root: &Utf8Path,
    patterns: &[String],
) -> io::Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    for pattern in patterns {
        let normalized = normalize_repo_relative_path(pattern);
        validate_repo_relative_path("inputs.paths", normalized)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        if looks_like_glob(normalized) {
            let glob = Glob::new(normalized).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid inputs.paths glob `{normalized}`: {error}"),
                )
            })?;
            let matcher = glob.compile_matcher();
            let mut matched = false;
            for relative in walk_repo_files(flake_root)? {
                if matcher.is_match(relative.as_str()) {
                    matched = true;
                    let digest = digest_repo_path(flake_root, relative.as_str())?;
                    digests.insert(relative.to_string(), digest);
                }
            }
            if !matched {
                digests.insert(
                    normalized.to_owned(),
                    digest_repo_path(flake_root, normalized)?,
                );
            }
        } else {
            let digest = digest_repo_path(flake_root, normalized)?;
            digests.insert(normalized.to_owned(), digest);
        }
    }
    Ok(digests)
}

fn walk_repo_files(flake_root: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
    let mut files = Vec::new();
    collect_repo_files(flake_root.as_std_path(), &mut files)?;
    files.sort();
    Ok(files
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(flake_root.as_std_path())
                .ok()
                .and_then(|relative| Utf8PathBuf::from_path_buf(relative.to_path_buf()).ok())
        })
        .collect())
}

fn collect_repo_files(current: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if current.is_file() {
        out.push(current.to_path_buf());
        return Ok(());
    }
    if !current.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == OsStr::new(".git") {
            continue;
        }
        collect_repo_files(&path, out)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EnvInputState {
    Unset,
    Empty,
    ValueDigest(String),
    Secret,
}

fn env_input_metadata(input: &EnvInput) -> (String, bool, bool) {
    match input {
        EnvInput::Name(name) => (name.clone(), false, false),
        EnvInput::Binding(binding) => (binding.name.clone(), binding.required, binding.secret),
    }
}

fn env_input_state(name: &str) -> io::Result<EnvInputState> {
    env_input_state_with(name, |var| std::env::var(var))
}

fn env_input_state_with(
    name: &str,
    lookup: impl FnOnce(&str) -> Result<String, std::env::VarError>,
) -> io::Result<EnvInputState> {
    match lookup(name) {
        Ok(value) if value.is_empty() => Ok(EnvInputState::Empty),
        Ok(value) => Ok(EnvInputState::ValueDigest(digest_bytes(value.as_bytes()))),
        Err(std::env::VarError::NotPresent) => Ok(EnvInputState::Unset),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn env_state_label(state: &EnvInputState) -> String {
    match state {
        EnvInputState::Unset => "unset".to_owned(),
        EnvInputState::Empty => "empty".to_owned(),
        EnvInputState::ValueDigest(digest) => format!("digest:{digest}"),
        EnvInputState::Secret => "secret".to_owned(),
    }
}

fn compute_git_state_digest(flake_root: &Utf8Path) -> io::Result<Option<String>> {
    let head = Command::new("git")
        .args(["-C", flake_root.as_str(), "rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() {
        return Ok(None);
    }
    let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head_sha.is_empty() {
        return Ok(None);
    }

    let status = Command::new("git")
        .args(["-C", flake_root.as_str(), "status", "--porcelain"])
        .output()?;
    if !status.status.success() {
        return Ok(None);
    }
    let dirty_digest = digest_bytes(&status.stdout);
    Ok(Some(digest_bytes(
        format!("{head_sha}:{dirty_digest}").as_bytes(),
    )))
}

fn output_contract_material(outputs: &[TaskOutput]) -> Vec<OutputMaterial> {
    outputs
        .iter()
        .map(|output| OutputMaterial {
            path: output.path.clone(),
            mode: output.mode.as_ref().map(|mode| format!("{mode:?}")),
            optional: output.optional,
        })
        .collect()
}

fn command_material(options: &WorkspaceCachePlanOptions) -> Option<CommandMaterial> {
    let program = options.command_program.clone()?;
    Some(CommandMaterial {
        program,
        argv: options.command_argv.clone(),
    })
}

fn secret_provider_label(provider: crate::schema::SecretProvider) -> &'static str {
    match provider {
        crate::schema::SecretProvider::Env => "env",
        crate::schema::SecretProvider::File => "file",
        crate::schema::SecretProvider::Sops => "sops",
        crate::schema::SecretProvider::SopsNix => "sops-nix",
    }
}

#[derive(Serialize)]
struct KeyMaterial {
    protocol_version: u32,
    schema_major: u32,
    task_id: String,
    task_definition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_salt: Option<String>,
    system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flake_lock_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flake_nix_digest: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    discovery_inputs: Vec<String>,
    app: String,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment_policy: Option<EnvironmentPolicy>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_secrets: Vec<ContextSecretMaterial>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    context_spawn_env_set: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<CommandMaterial>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    forwarded_args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    path_inputs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env_inputs: BTreeMap<String, EnvInputState>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    binding_inputs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_state_digest: Option<String>,
    outputs: Vec<OutputMaterial>,
}

#[derive(Serialize)]
struct CommandMaterial {
    program: String,
    argv: Vec<String>,
}

#[derive(Serialize)]
struct OutputMaterial {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    optional: bool,
}

#[derive(Serialize)]
struct ContextSecretMaterial {
    name: String,
    reference: String,
    delivery: String,
    provider: String,
}

impl From<&PlanSecretEntry> for ContextSecretMaterial {
    fn from(entry: &PlanSecretEntry) -> Self {
        Self {
            name: entry.name.clone(),
            reference: entry.reference.clone(),
            delivery: format!("{:?}", entry.delivery),
            provider: secret_provider_label(entry.provider).to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        EnvInputBinding, TaskCache, TaskCacheMode, TaskDefinition, TaskDocument, TaskInputBinding,
        TaskInputs, TaskOutput,
    };

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
            secret_policy: None,
        });
        def
    }

    fn plan_options() -> WorkspaceCachePlanOptions {
        WorkspaceCachePlanOptions {
            command_program: Some("/nix/store/nix/bin/nix".to_owned()),
            command_argv: vec![
                "run".to_owned(),
                ".#codegen".to_owned(),
                "--".to_owned(),
                "extra".to_owned(),
            ],
            forwarded_args: vec!["extra".to_owned()],
            ..WorkspaceCachePlanOptions::default()
        }
    }

    fn empty_doc() -> TaskDocument {
        TaskDocument {
            schema_version: 2,
            tasks: BTreeMap::new(),
            apps: BTreeMap::new(),
            discovery_inputs: Vec::new(),
            contexts: BTreeMap::new(),
            processes: BTreeMap::new(),
        }
    }

    #[test]
    fn derivation_task_has_no_action_key() {
        let doc = empty_doc();
        let def = TaskDefinition::new("test");
        let plan = build_workspace_cache_plan(
            &doc,
            "test",
            &def,
            "aarch64-darwin",
            camino::Utf8Path::new("/tmp"),
            "/tmp",
            &BTreeMap::new(),
            &WorkspaceCachePlanOptions::default(),
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
        let doc = empty_doc();
        let def = workspace_task();
        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().join("proj").to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan");
        assert_eq!(plan.tier, ActionTier::WorkspaceAction);
        assert!(plan.cache_enabled);
        assert!(plan.action_key.is_some());
    }

    #[test]
    fn changing_forwarded_args_changes_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let def = workspace_task();
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).expect("mkdir");

        let mut options_a = plan_options();
        let plan_a = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            cwd.as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &options_a,
        )
        .expect("plan a");

        options_a.forwarded_args = vec!["other".to_owned()];
        options_a.command_argv = vec![
            "run".to_owned(),
            ".#codegen".to_owned(),
            "--".to_owned(),
            "other".to_owned(),
        ];
        let plan_b = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            cwd.as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &options_a,
        )
        .expect("plan b");

        assert_ne!(plan_a.action_key, plan_b.action_key);
    }

    #[test]
    fn changing_task_definition_changes_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let def_a = workspace_task();
        let mut def_b = workspace_task();
        def_b.description = Some("different".to_owned());
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).expect("mkdir");

        let plan_a = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def_a,
            "aarch64-darwin",
            &flake,
            cwd.as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan a");
        let plan_b = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def_b,
            "aarch64-darwin",
            &flake,
            cwd.as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan b");

        assert_ne!(plan_a.action_key, plan_b.action_key);
    }

    #[test]
    fn relative_cwd_stable_across_different_absolute_roots() {
        let outer = tempfile::tempdir().expect("outer");
        let checkout_a = outer.path().join("checkout-a");
        let checkout_b = outer.path().join("checkout-b");
        std::fs::create_dir_all(checkout_a.join("proj")).expect("mkdir a");
        std::fs::create_dir_all(checkout_b.join("proj")).expect("mkdir b");

        let flake_a = Utf8PathBuf::from_path_buf(checkout_a.clone()).expect("utf8");
        let flake_b = Utf8PathBuf::from_path_buf(checkout_b.clone()).expect("utf8");
        let doc = empty_doc();
        let def = workspace_task();
        let options = plan_options();

        let plan_a = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake_a,
            checkout_a.join("proj").as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &options,
        )
        .expect("plan a");
        let plan_b = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake_b,
            checkout_b.join("proj").as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &options,
        )
        .expect("plan b");

        assert_eq!(plan_a.action_key, plan_b.action_key);
        assert_eq!(plan_a.key_components.get("cwd"), Some(&"proj".to_owned()));
    }

    #[test]
    fn secret_env_values_not_in_key_material() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let mut def = workspace_task();
        def.inputs = Some(TaskInputs {
            env: vec![
                EnvInput::Binding(EnvInputBinding {
                    name: "NXR_TEST_SECRET_ENV".to_owned(),
                    required: false,
                    secret: true,
                }),
                EnvInput::Name("NXR_TEST_PUBLIC_ENV".to_owned()),
            ],
            ..TaskInputs::default()
        });

        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan");
        assert!(!plan.cache_enabled);
        assert!(plan.action_key.is_none());
        assert_eq!(
            plan.key_components.get("cache_disabled"),
            Some(&"secret-bearing task disables workspace cache by default".to_owned())
        );
        let components = serde_json::to_string(&plan.key_components).expect("json");
        assert!(!components.contains("super-secret-value"));
    }

    #[test]
    fn secret_env_ignore_values_keeps_cache_enabled_without_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let mut def = workspace_task();
        def.cache.as_mut().expect("cache").secret_policy =
            Some(crate::schema::TaskCacheSecretPolicy::IgnoreValues);
        def.inputs = Some(TaskInputs {
            env: vec![EnvInput::Binding(EnvInputBinding {
                name: "NXR_TEST_SECRET_ENV".to_owned(),
                required: false,
                secret: true,
            })],
            ..TaskInputs::default()
        });

        let plan_a = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan a");
        let plan_b = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan b");

        assert!(plan_a.cache_enabled);
        assert_eq!(plan_a.action_key, plan_b.action_key);
        let components = serde_json::to_string(&plan_a.key_components).expect("json");
        assert!(!components.contains("super-secret-value"));
        assert_eq!(
            plan_a.key_components.get("input.env:NXR_TEST_SECRET_ENV"),
            Some(&"secret".to_owned())
        );
    }

    #[test]
    fn context_secrets_disable_cache_by_default() {
        use crate::context::{PlanSecretEntry, PlanSecretValuePlaceholder};
        use crate::schema::{SecretDelivery, SecretProvider};

        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let def = workspace_task();
        let mut options = plan_options();
        options.context_name = Some("release".to_owned());
        options.context_secrets = vec![PlanSecretEntry {
            name: "DEPLOY_TOKEN".to_owned(),
            reference: "NXR_FIXTURE_DEPLOY_TOKEN".to_owned(),
            provider: SecretProvider::Env,
            delivery: SecretDelivery::Env,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }];

        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &options,
        )
        .expect("plan");

        assert!(!plan.cache_enabled);
        assert_eq!(
            plan.key_components.get("cache_disabled"),
            Some(&"secret-bearing task disables workspace cache by default".to_owned())
        );
    }

    #[test]
    fn shared_cache_mode_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let mut def = workspace_task();
        def.cache.as_mut().expect("cache").mode = Some(TaskCacheMode::Shared);

        let err = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect_err("shared must fail closed");
        assert!(err.to_string().contains("not implemented"));
        assert!(err.to_string().contains("shared"));
    }

    #[test]
    fn env_input_state_uses_digest_not_raw_value() {
        let state = env_input_state_with("NXR_TEST_PUBLIC_ENV", |_| Ok("visible-value".to_owned()))
            .expect("state");
        assert!(matches!(state, EnvInputState::ValueDigest(_)));
        let label = env_state_label(&state);
        assert!(!label.contains("visible-value"));
        assert!(label.starts_with("digest:"));
    }

    #[test]
    fn unresolved_upstream_disables_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let mut def = workspace_task();
        def.inputs = Some(TaskInputs {
            bindings: BTreeMap::from([(
                "artifact".to_owned(),
                TaskInputBinding {
                    from: "upstream.out".to_owned(),
                },
            )]),
            ..TaskInputs::default()
        });

        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan");

        assert!(!plan.cache_enabled);
        assert!(plan.action_key.is_none());
        assert_eq!(
            plan.key_components.get("cache_disabled"),
            Some(&"unresolved upstream binding".to_owned())
        );
    }

    #[test]
    fn required_missing_env_disables_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let flake = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let doc = empty_doc();
        let mut def = workspace_task();
        def.inputs = Some(TaskInputs {
            env: vec![EnvInput::Binding(EnvInputBinding {
                name: "NXR_ACTION_KEY_TEST_REQUIRED_MISSING_7f3a".to_owned(),
                required: true,
                secret: false,
            })],
            ..TaskInputs::default()
        });

        let plan = build_workspace_cache_plan(
            &doc,
            "codegen",
            &def,
            "aarch64-darwin",
            &flake,
            tmp.path().as_os_str().to_str().expect("utf8"),
            &BTreeMap::new(),
            &plan_options(),
        )
        .expect("plan");

        assert!(!plan.cache_enabled);
        assert!(plan.action_key.is_none());
        assert_eq!(
            plan.key_components.get("cache_disabled"),
            Some(&"required environment input missing".to_owned())
        );
    }
}
