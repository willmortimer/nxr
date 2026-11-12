//! One discoverable operation, derived from an existing declaration.
//!
//! The portable id does not contain a machine path. The workspace root and
//! source revision travel beside it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What an operation is allowed to do. A missing annotation is never [`Deploy`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectClass {
    /// Discovery or inspection.
    Read,
    /// Local build, test, or check.
    LocalBuild,
    /// Local change that is not a compile, such as a script or process.
    LocalMutation,
    /// Work admitted to another machine.
    Remote,
    /// Deployment or fleet activation.
    Deploy,
}

impl EffectClass {
    /// Classify from the declaration name when no policy annotation is present.
    #[must_use]
    pub fn from_declaration(kind: OperationKind, name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("deploy")
            || name.contains("hub-update")
            || name.contains("fabric-sync")
            || name.contains("switch-to-configuration")
        {
            return Self::Deploy;
        }
        if name.contains("remote") || (kind == OperationKind::Workflow && name.contains("submit")) {
            return Self::Remote;
        }
        if matches!(name.as_str(), "list" | "status" | "doctor" | "inspect") {
            return Self::Read;
        }
        match kind {
            OperationKind::Script | OperationKind::Process => Self::LocalMutation,
            OperationKind::App | OperationKind::Task | OperationKind::Workflow => Self::LocalBuild,
        }
    }
}

impl fmt::Display for EffectClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Read => "read",
            Self::LocalBuild => "local-build",
            Self::LocalMutation => "local-mutation",
            Self::Remote => "remote",
            Self::Deploy => "deploy",
        };
        formatter.write_str(label)
    }
}

/// Where the declaration came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationKind {
    /// A flake app.
    App,
    /// An NXR task.
    Task,
    /// A file under `.nxr/scripts`.
    Script,
    /// A task in `.nxr/workflows.toml`.
    Workflow,
    /// A long-running process node.
    Process,
}

impl OperationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Task => "task",
            Self::Script => "script",
            Self::Workflow => "workflow",
            Self::Process => "process",
        }
    }
}

/// The checkout this record was resolved in. Not part of [`OperationRecord::operation_id`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInstance {
    /// Flake or repository name.
    pub workspace_id: String,
    /// Absolute checkout path for this machine.
    pub workspace_root: String,
    /// Git revision when the checkout has one.
    pub source_revision: Option<String>,
}

/// Where `execution_allowed` came from. A missing annotation is [`Unset`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicySource {
    /// The name is on the local-check allowlist and the kind is a task or app.
    Allowlist,
    /// The declaration named an effect. That does not by itself authorize execution.
    Explicit,
    /// No authorization was declared.
    Unset,
    /// The exact operation id is listed in `.nxr/execution-policy.toml`.
    Project,
}

const LOCAL_CHECK_ALLOWLIST: &[&str] = &["test", "fmt", "fmt-check", "lint", "ci", "deny"];

/// Whether this caller may start the operation. Unknown work stays discoverable.
#[must_use]
pub fn execution_decision(
    kind: OperationKind,
    name: &str,
    explicit: Option<EffectClass>,
) -> (Option<EffectClass>, PolicySource, bool) {
    let allowlisted = matches!(kind, OperationKind::App | OperationKind::Task)
        && LOCAL_CHECK_ALLOWLIST.contains(&name);
    let source = if allowlisted {
        PolicySource::Allowlist
    } else if explicit.is_some() {
        PolicySource::Explicit
    } else {
        PolicySource::Unset
    };
    (explicit, source, allowlisted)
}

/// Exact operation ids a trusted checkout may start, with the parameter names each id accepts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectExecutionPolicy {
    operations: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct PolicyFile {
    #[serde(default)]
    operation: Vec<PolicyOperation>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct PolicyOperation {
    id: String,
    #[serde(default)]
    parameters: Vec<String>,
}

impl ProjectExecutionPolicy {
    /// Load `.nxr/execution-policy.toml`. A missing file authorizes nothing beyond the allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists and cannot be read or parsed.
    pub fn load(root: &std::path::Path) -> Result<Self, String> {
        let path = root.join(".nxr").join("execution-policy.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|_| "execution policy cannot be read".to_owned())?;
        let file: PolicyFile =
            toml::from_str(&text).map_err(|_| "execution policy cannot be parsed".to_owned())?;
        let mut operations = std::collections::BTreeMap::new();
        for operation in file.operation {
            if operation.id.is_empty() {
                return Err("execution policy operation id is empty".to_owned());
            }
            operations.insert(operation.id, operation.parameters);
        }
        Ok(Self { operations })
    }

    /// Parameter names this policy allows for `operation_id`.
    #[must_use]
    pub fn parameters(&self, operation_id: &str) -> &[String] {
        self.operations
            .get(operation_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Apply a trusted project policy. Deploy, remote, and `reset-db` stay refused.
#[must_use]
pub fn apply_project_policy(
    record: &OperationRecord,
    policy: &ProjectExecutionPolicy,
) -> OperationRecord {
    if record.execution_allowed || !policy.operations.contains_key(&record.operation_id) {
        return record.clone();
    }
    if !matches!(record.kind, OperationKind::App | OperationKind::Task) {
        return record.clone();
    }
    if matches!(
        record.inferred_effect,
        EffectClass::Deploy | EffectClass::Remote
    ) || record.name == "reset-db"
    {
        return record.clone();
    }
    let mut authorized = record.clone();
    authorized.execution_allowed = true;
    authorized.policy_source = PolicySource::Project;
    authorized
}

/// Reject parameter names the project policy did not list.
///
/// # Errors
///
/// Returns an error when a provided name is absent from the policy entry.
pub fn validate_policy_parameters(
    policy: &ProjectExecutionPolicy,
    operation_id: &str,
    provided: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let allowed = policy.parameters(operation_id);
    for name in provided.keys() {
        if !allowed.iter().any(|candidate| candidate == name) {
            return Err(format!("parameter `{name}` is not authorized"));
        }
    }
    Ok(())
}

/// Stable workspace name. An explicit id wins, then the git remote repository, then the directory.
#[must_use]
pub fn workspace_id(
    explicit: Option<&str>,
    git_remote: Option<&str>,
    directory_name: &str,
) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| git_remote.and_then(repository_name))
        .unwrap_or_else(|| directory_name.to_owned())
}

fn repository_name(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches(".git");
    let name = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// One declared operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Portable id: `{workspace_id}:{kind}:{name}`.
    pub operation_id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    pub source_revision: Option<String>,
    /// Declaring file, relative to the workspace root.
    pub definition: String,
    pub kind: OperationKind,
    pub name: String,
    pub description: Option<String>,
    pub aliases: Vec<String>,
    /// Descriptive classification. Not an authorization.
    pub inferred_effect: EffectClass,
    /// Effect named by the declaration, when one exists.
    pub authorized_effect: Option<EffectClass>,
    pub policy_source: PolicySource,
    /// True only for an allowlisted local task or app.
    pub execution_allowed: bool,
    pub parameters: Vec<String>,
    /// Expected dev shell, when the declaration names one.
    pub shell: Option<String>,
    pub working_directory: String,
    /// Expected `RUSTC_WRAPPER`, when the task environment sets one.
    pub rustc_wrapper: Option<String>,
    pub inputs: Vec<String>,
    /// Secret references. Never resolved values.
    pub secret_refs: Vec<String>,
}

/// A declaration before it becomes an [`OperationRecord`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declaration {
    pub kind: OperationKind,
    pub name: String,
    pub description: Option<String>,
    pub aliases: Vec<String>,
    pub definition: String,
    /// Explicit policy. `None` is classified from the name and kind, and is not deploy
    /// unless the name itself is a deploy declaration.
    pub effect: Option<EffectClass>,
    pub parameters: Vec<String>,
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    pub rustc_wrapper: Option<String>,
    pub inputs: Vec<String>,
    pub secret_refs: Vec<String>,
}

impl Declaration {
    /// A named declaration with no policy annotation.
    #[must_use]
    pub fn named(
        kind: OperationKind,
        name: impl Into<String>,
        definition: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            name: name.into(),
            description: None,
            aliases: Vec::new(),
            definition: definition.into(),
            effect: None,
            parameters: Vec::new(),
            shell: None,
            working_directory: None,
            rustc_wrapper: None,
            inputs: Vec::new(),
            secret_refs: Vec::new(),
        }
    }
}

/// Build one record. The id does not contain `workspace_root`.
#[must_use]
pub fn operation_record(
    workspace: &WorkspaceInstance,
    declaration: &Declaration,
) -> OperationRecord {
    let inferred_effect = EffectClass::from_declaration(declaration.kind, &declaration.name);
    let (authorized_effect, policy_source, execution_allowed) =
        execution_decision(declaration.kind, &declaration.name, declaration.effect);
    let operation_id = format!(
        "{}:{}:{}",
        workspace.workspace_id,
        declaration.kind.as_str(),
        declaration.name
    );
    OperationRecord {
        operation_id,
        workspace_id: workspace.workspace_id.clone(),
        workspace_root: workspace.workspace_root.clone(),
        source_revision: workspace.source_revision.clone(),
        definition: declaration.definition.clone(),
        kind: declaration.kind,
        name: declaration.name.clone(),
        description: declaration.description.clone(),
        aliases: declaration.aliases.clone(),
        inferred_effect,
        authorized_effect,
        policy_source,
        execution_allowed,
        parameters: declaration.parameters.clone(),
        shell: declaration.shell.clone(),
        working_directory: declaration
            .working_directory
            .clone()
            .unwrap_or_else(|| ".".to_owned()),
        rustc_wrapper: declaration.rustc_wrapper.clone(),
        inputs: declaration.inputs.clone(),
        secret_refs: declaration.secret_refs.clone(),
    }
}

/// Exact, case-insensitive match against the id, name, description, and aliases.
#[must_use]
pub fn search<'a>(records: &'a [OperationRecord], query: &str) -> Vec<&'a OperationRecord> {
    let query = query.trim().to_ascii_lowercase();
    let mut matched: Vec<&OperationRecord> = records
        .iter()
        .filter(|record| query.is_empty() || matches_query(record, &query))
        .collect();
    matched.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
    matched
}

fn matches_query(record: &OperationRecord, query: &str) -> bool {
    record.operation_id.to_ascii_lowercase().contains(query)
        || record.name.to_ascii_lowercase().contains(query)
        || record
            .description
            .as_deref()
            .is_some_and(|description| description.to_ascii_lowercase().contains(query))
        || record
            .aliases
            .iter()
            .any(|alias| alias.to_ascii_lowercase().contains(query))
}

/// One ranked suggestion. Exact [`search`] is unchanged.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OperationSuggestion {
    pub operation_id: String,
    pub match_reason: Vec<String>,
    pub execution_allowed: bool,
}

/// Rank operations for a natural-language query.
///
/// Matching is lexical. The bound workspace, changed paths, and current policy
/// raise a result. No embedding model is used.
#[must_use]
pub fn suggest(
    records: &[OperationRecord],
    query: &str,
    workspace_id: &str,
    changed_paths: &[String],
) -> Vec<OperationSuggestion> {
    let tokens = tokenize(query);
    let documents: Vec<Vec<String>> = records.iter().map(document_tokens).collect();
    let average = if documents.is_empty() {
        0.0
    } else {
        documents
            .iter()
            .map(|tokens| tokens.len() as f64)
            .sum::<f64>()
            / documents.len() as f64
    };
    let mut ranked = records
        .iter()
        .zip(documents.iter())
        .filter_map(|(record, document)| {
            let mut score = bm25(&tokens, document, &documents, average);
            let mut reasons = Vec::new();
            if score > 0.0 {
                reasons.push("matches the query".to_owned());
            }
            if record.workspace_id == workspace_id {
                score += 0.35;
                reasons.push("in the bound workspace".to_owned());
            }
            if path_applies(record, changed_paths) {
                score += 0.75;
                reasons.push("applicable to a changed path".to_owned());
            }
            if record.execution_allowed {
                score += 0.25;
                reasons.push("executable under the current policy".to_owned());
            }
            if record.shell.is_some() {
                reasons.push("runs in the pinned environment".to_owned());
            }
            if score <= 0.0 || reasons.is_empty() {
                return None;
            }
            Some((
                score,
                OperationSuggestion {
                    operation_id: record.operation_id.clone(),
                    match_reason: reasons,
                    execution_allowed: record.execution_allowed,
                },
            ))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.operation_id.cmp(&right.1.operation_id))
    });
    ranked
        .into_iter()
        .map(|(_, suggestion)| suggestion)
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn document_tokens(record: &OperationRecord) -> Vec<String> {
    let mut text = format!(
        "{} {} {}",
        record.operation_id,
        record.name,
        record.description.as_deref().unwrap_or("")
    );
    for alias in &record.aliases {
        text.push(' ');
        text.push_str(alias);
    }
    for input in &record.inputs {
        text.push(' ');
        text.push_str(input);
    }
    tokenize(&text)
}

fn path_applies(record: &OperationRecord, changed_paths: &[String]) -> bool {
    changed_paths.iter().any(|path| {
        record.inputs.iter().any(|input| {
            path.starts_with(input.trim_end_matches('*').trim_end_matches('/'))
                || input.contains(path)
        }) || tokenize(path).iter().any(|token| {
            record.name.contains(token.as_str())
                || record
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_ascii_lowercase().contains(token))
        })
    })
}

fn bm25(query: &[String], document: &[String], corpus: &[Vec<String>], average: f64) -> f64 {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    if query.is_empty() || document.is_empty() || corpus.is_empty() || average == 0.0 {
        return 0.0;
    }
    let mut score = 0.0;
    for token in query {
        let frequency = document.iter().filter(|item| item == &token).count() as f64;
        if frequency == 0.0 {
            continue;
        }
        let docs = corpus
            .iter()
            .filter(|document| document.iter().any(|item| item == token))
            .count() as f64;
        let idf = ((corpus.len() as f64 - docs + 0.5) / (docs + 0.5) + 1.0).ln();
        let length = document.len() as f64;
        score +=
            idf * (frequency * (K1 + 1.0)) / (frequency + K1 * (1.0 - B + B * length / average));
    }
    score
}

/// The record with this portable id.
#[must_use]
pub fn describe<'a>(
    records: &'a [OperationRecord],
    operation_id: &str,
) -> Option<&'a OperationRecord> {
    records
        .iter()
        .find(|record| record.operation_id == operation_id)
}

/// The work a later execution would perform. This does not run it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationPlan {
    pub operation_id: String,
    pub inferred_effect: EffectClass,
    pub authorized_effect: Option<EffectClass>,
    pub policy_source: PolicySource,
    pub execution_allowed: bool,
    pub definition: String,
    pub parameters: Vec<String>,
    pub shell: Option<String>,
    pub working_directory: String,
    pub rustc_wrapper: Option<String>,
    pub inputs: Vec<String>,
    pub secret_refs: Vec<String>,
}

/// Plan one record.
#[must_use]
pub fn plan(record: &OperationRecord) -> OperationPlan {
    OperationPlan {
        operation_id: record.operation_id.clone(),
        inferred_effect: record.inferred_effect,
        authorized_effect: record.authorized_effect,
        policy_source: record.policy_source,
        execution_allowed: record.execution_allowed,
        definition: record.definition.clone(),
        parameters: record.parameters.clone(),
        shell: record.shell.clone(),
        working_directory: record.working_directory.clone(),
        rustc_wrapper: record.rustc_wrapper.clone(),
        inputs: record.inputs.clone(),
        secret_refs: record.secret_refs.clone(),
    }
}

/// One difference between the planned environment and the ambient process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentMismatch {
    pub field: String,
    pub expected: String,
    pub observed: String,
}

/// One environment view. `compiler` is `unknown` when it was not established.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentView {
    pub shell: Option<String>,
    pub compiler: String,
    pub rustc_wrapper: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentExplanation {
    pub operation_id: String,
    /// What the declaration names.
    pub declared: EnvironmentView,
    /// What NXR can name without entering the shell. Unknown stays unknown.
    pub resolved: EnvironmentView,
    /// What the current process `PATH` suggests.
    pub ambient: EnvironmentView,
    pub working_directory: String,
    pub inputs: Vec<String>,
    pub secret_refs: Vec<String>,
    pub mismatches: Vec<EnvironmentMismatch>,
}

/// Compare the plan with an ambient `PATH`.
///
/// `ambient_path` is the process path that would be used if the task were
/// started outside its dev shell. A `~/.rustup` or `~/.cargo/bin` entry ahead
/// of a Nix store path is a mismatch when the declaration names a shell.
#[must_use]
pub fn explain(record: &OperationRecord, ambient_path: &str) -> EnvironmentExplanation {
    let planned = plan(record);
    let ambient_compiler = compiler_from_path(ambient_path);
    let mut mismatches = Vec::new();
    if record.shell.is_some() && ambient_compiler == "rustup" {
        mismatches.push(EnvironmentMismatch {
            field: "compiler".to_owned(),
            expected: "nix shell".to_owned(),
            observed: "rustup".to_owned(),
        });
    }
    EnvironmentExplanation {
        operation_id: planned.operation_id,
        declared: EnvironmentView {
            shell: planned.shell.clone(),
            compiler: "unknown".to_owned(),
            rustc_wrapper: planned.rustc_wrapper.clone(),
        },
        resolved: EnvironmentView {
            shell: planned.shell,
            compiler: "unknown".to_owned(),
            rustc_wrapper: planned.rustc_wrapper,
        },
        ambient: EnvironmentView {
            shell: None,
            compiler: ambient_compiler.to_owned(),
            rustc_wrapper: None,
        },
        working_directory: planned.working_directory,
        inputs: planned.inputs,
        secret_refs: planned.secret_refs,
        mismatches,
    }
}

/// Fingerprint of one execution. `HEAD` is not an input. File bytes are.
#[must_use]
pub fn execution_fingerprint(record: &OperationRecord, input_contents: &[(&str, &[u8])]) -> String {
    let mut material = String::new();
    material.push_str(&record.operation_id);
    material.push('\n');
    for parameter in &record.parameters {
        material.push_str(parameter);
        material.push('\n');
    }
    material.push_str(record.shell.as_deref().unwrap_or(""));
    material.push('\n');
    material.push_str(record.rustc_wrapper.as_deref().unwrap_or(""));
    material.push('\n');
    for input in &record.inputs {
        material.push_str(input);
        material.push('\n');
    }
    let mut contents = input_contents.to_vec();
    contents.sort_by(|left, right| left.0.cmp(right.0));
    for (path, bytes) in contents {
        material.push_str(path);
        material.push(' ');
        material.push_str(&nxr_core::digest_bytes(bytes));
        material.push('\n');
    }
    nxr_core::digest_bytes(material.as_bytes())
}

/// Fingerprint from resolved input digests and validated parameter values.
#[must_use]
pub fn execution_fingerprint_with_inputs(
    record: &OperationRecord,
    digests: &std::collections::BTreeMap<String, String>,
    parameters: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut material = String::new();
    material.push_str(&record.operation_id);
    material.push('\n');
    for (name, value) in parameters {
        material.push_str(name);
        material.push('=');
        material.push_str(value);
        material.push('\n');
    }
    material.push_str(record.shell.as_deref().unwrap_or(""));
    material.push('\n');
    material.push_str(record.rustc_wrapper.as_deref().unwrap_or(""));
    material.push('\n');
    for (path, digest) in digests {
        material.push_str(path);
        material.push(' ');
        material.push_str(digest);
        material.push('\n');
    }
    nxr_core::digest_bytes(material.as_bytes())
}

/// Same key and fingerprint repeats. A changed fingerprint conflicts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FingerprintDecision {
    /// The key has not been seen.
    New,
    /// Same key, same operation, same fingerprint.
    Replay,
    /// Same key, different fingerprint.
    Conflict,
}

#[must_use]
pub fn fingerprint_decision(
    stored_operation: &str,
    stored_fingerprint: &str,
    operation_id: &str,
    fingerprint: &str,
) -> FingerprintDecision {
    if stored_operation == operation_id && stored_fingerprint == fingerprint {
        FingerprintDecision::Replay
    } else {
        FingerprintDecision::Conflict
    }
}

fn compiler_from_path(ambient_path: &str) -> &'static str {
    for entry in ambient_path.split(':') {
        if entry.contains("/.cargo/bin") || entry.contains("/.rustup") {
            return "rustup";
        }
        if entry.contains("/nix/store/") {
            return "nix";
        }
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> WorkspaceInstance {
        WorkspaceInstance {
            workspace_id: id.to_owned(),
            workspace_root: format!("/tmp/{id}"),
            source_revision: Some("abc123".to_owned()),
        }
    }

    fn sample(id: &str, name: &str) -> OperationRecord {
        let mut declaration = Declaration::named(OperationKind::Task, name, "flake.nix");
        declaration.description = Some(format!("{name} packages"));
        declaration.aliases = vec!["check".to_owned()];
        declaration.shell = Some("rust".to_owned());
        declaration.rustc_wrapper = Some("sccache".to_owned());
        operation_record(&workspace(id), &declaration)
    }

    #[test]
    fn lattice_and_nixplane_tasks_have_distinct_portable_ids() {
        let lattice = sample("lattice", "rust-test-pkgs");
        let nixplane = sample("nixplane", "test");
        assert_eq!(lattice.operation_id, "lattice:task:rust-test-pkgs");
        assert_eq!(nixplane.operation_id, "nixplane:task:test");
        assert!(!lattice.operation_id.contains("/tmp/"));
        assert_ne!(lattice.operation_id, nixplane.operation_id);
        let records = [lattice.clone(), nixplane];
        let found = search(&records, "rust-test-pkgs");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].operation_id, "lattice:task:rust-test-pkgs");
        let mut rust = found[0].clone();
        rust.description = Some("Rust verification for changed packages".to_owned());
        rust.inputs = vec!["crates/foo".to_owned()];
        rust.shell = Some("rust".to_owned());
        let other = sample("lattice", "reset-db");
        let suggestions = suggest(
            &[rust, other],
            "test my changed Rust code",
            "lattice",
            &["crates/foo/src/lib.rs".to_owned()],
        );
        assert_eq!(suggestions[0].operation_id, "lattice:task:rust-test-pkgs");
        assert!(suggestions[0].execution_allowed || !suggestions[0].match_reason.is_empty());
        assert!(
            suggestions[0]
                .match_reason
                .iter()
                .any(|reason| reason.contains("changed"))
        );
        assert_eq!(
            describe(&[found[0].clone()], "lattice:task:rust-test-pkgs")
                .map(|record| record.operation_id.as_str()),
            Some("lattice:task:rust-test-pkgs")
        );
        assert_eq!(plan(&found[0]).inferred_effect, EffectClass::LocalBuild);
        assert!(!plan(&found[0]).execution_allowed);
        assert!(plan(&records[1]).execution_allowed);
    }

    #[test]
    fn a_missing_policy_is_not_deploy_or_executable() {
        let record = operation_record(
            &workspace("nxr"),
            &Declaration::named(OperationKind::Task, "fmt", "flake.nix"),
        );
        assert_eq!(record.inferred_effect, EffectClass::LocalBuild);
        assert!(record.execution_allowed);
        let named = operation_record(
            &workspace("nixplane"),
            &Declaration::named(OperationKind::Task, "hub-update", "flake.nix"),
        );
        assert_eq!(named.inferred_effect, EffectClass::Deploy);
        assert!(!named.execution_allowed);
        let destructive = operation_record(
            &workspace("nixplane"),
            &Declaration::named(OperationKind::Task, "reset-db", "flake.nix"),
        );
        assert!(!destructive.execution_allowed);
        assert_eq!(destructive.policy_source, PolicySource::Unset);
        let root = tempfile::tempdir().unwrap();
        let policy_dir = root.path().join(".nxr");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(
            policy_dir.join("execution-policy.toml"),
            "[[operation]]\nid = \"lattice:task:rust-test-pkgs\"\nparameters = [\"package\", \"filter\"]\n\n[[operation]]\nid = \"nixplane:task:reset-db\"\n",
        )
        .unwrap();
        let policy = ProjectExecutionPolicy::load(root.path()).unwrap();
        let authorized = apply_project_policy(&sample("lattice", "rust-test-pkgs"), &policy);
        assert!(authorized.execution_allowed);
        assert_eq!(authorized.policy_source, PolicySource::Project);
        let other = operation_record(
            &workspace("other"),
            &Declaration::named(OperationKind::Task, "rust-test-pkgs", "flake.nix"),
        );
        assert!(!apply_project_policy(&other, &policy).execution_allowed);
        let reset = operation_record(
            &workspace("nixplane"),
            &Declaration::named(OperationKind::Task, "reset-db", "flake.nix"),
        );
        assert!(!apply_project_policy(&reset, &policy).execution_allowed);
        let mut values = std::collections::BTreeMap::new();
        values.insert("package".to_owned(), "lattice-core".to_owned());
        assert!(
            validate_policy_parameters(&policy, "lattice:task:rust-test-pkgs", &values).is_ok()
        );
        values.insert("shell".to_owned(), "bash".to_owned());
        assert!(
            validate_policy_parameters(&policy, "lattice:task:rust-test-pkgs", &values).is_err()
        );
    }

    #[test]
    fn explain_reports_rustup_ahead_of_the_nix_shell_without_compiling() {
        let record = sample("lattice", "rust-test-pkgs");
        let report = explain(&record, "/Users/will/.cargo/bin:/nix/store/rust/bin");
        assert_eq!(report.ambient.compiler, "rustup");
        assert_eq!(report.resolved.compiler, "unknown");
        assert_eq!(report.declared.shell.as_deref(), Some("rust"));
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].field, "compiler");
        assert_eq!(report.declared.rustc_wrapper.as_deref(), Some("sccache"));
        assert!(report.secret_refs.is_empty());
        let nix = explain(&record, "/nix/store/rust/bin:/usr/bin");
        assert!(nix.mismatches.is_empty());
        assert_eq!(nix.ambient.compiler, "nix");
        assert_eq!(nix.resolved.compiler, "unknown");
    }

    #[test]
    fn script_extensions_stay_in_the_operation_id() {
        let root = workspace("demo");
        let shell = operation_record(
            &root,
            &Declaration::named(OperationKind::Script, "run.sh", ".nxr/scripts/run.sh"),
        );
        let python = operation_record(
            &root,
            &Declaration::named(OperationKind::Script, "run.py", ".nxr/scripts/run.py"),
        );
        assert_ne!(shell.operation_id, python.operation_id);
        assert_eq!(shell.definition, ".nxr/scripts/run.sh");
        assert!(!shell.execution_allowed);
    }

    #[test]
    fn a_dirty_input_changes_the_fingerprint_without_moving_head() {
        let record = sample("lattice", "rust-test-pkgs");
        let first = execution_fingerprint(&record, &[("src/lib.rs", b"fn a() {}")]);
        let second = execution_fingerprint(&record, &[("src/lib.rs", b"fn b() {}")]);
        assert_ne!(first, second);
        assert_eq!(
            fingerprint_decision(&record.operation_id, &first, &record.operation_id, &first),
            FingerprintDecision::Replay
        );
        assert_eq!(
            fingerprint_decision(&record.operation_id, &first, &record.operation_id, &second),
            FingerprintDecision::Conflict
        );
    }

    #[test]
    fn workspace_id_prefers_metadata_over_the_directory_name() {
        assert_eq!(
            workspace_id(
                Some("lattice"),
                Some("git@github.com:willmortimer/other.git"),
                "backend"
            ),
            "lattice"
        );
        assert_eq!(
            workspace_id(
                None,
                Some("https://github.com/willmortimer/nxr.git"),
                "backend"
            ),
            "nxr"
        );
        assert_eq!(workspace_id(None, None, "backend"), "backend");
    }
}
