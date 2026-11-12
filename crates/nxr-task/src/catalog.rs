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
    pub effect: EffectClass,
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
    let effect = declaration
        .effect
        .unwrap_or_else(|| EffectClass::from_declaration(declaration.kind, &declaration.name));
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
        effect,
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
    pub effect: EffectClass,
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
        effect: record.effect,
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

/// Environment the operation would use, plus drift. No compiler is launched.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentExplanation {
    pub operation_id: String,
    pub shell: Option<String>,
    pub compiler: String,
    pub rustc_wrapper: Option<String>,
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
    let mut mismatches = Vec::new();
    let compiler = compiler_from_path(ambient_path);
    if record.shell.is_some() && compiler == "rustup" {
        mismatches.push(EnvironmentMismatch {
            field: "compiler".to_owned(),
            expected: "nix shell".to_owned(),
            observed: "rustup".to_owned(),
        });
    }
    EnvironmentExplanation {
        operation_id: planned.operation_id,
        shell: planned.shell,
        compiler: compiler.to_owned(),
        rustc_wrapper: planned.rustc_wrapper,
        working_directory: planned.working_directory,
        inputs: planned.inputs,
        secret_refs: planned.secret_refs,
        mismatches,
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
        assert_eq!(
            describe(&[found[0].clone()], "lattice:task:rust-test-pkgs")
                .map(|record| record.operation_id.as_str()),
            Some("lattice:task:rust-test-pkgs")
        );
        assert_eq!(plan(&found[0]).effect, EffectClass::LocalBuild);
    }

    #[test]
    fn a_missing_policy_is_not_deploy() {
        let record = operation_record(
            &workspace("nxr"),
            &Declaration::named(OperationKind::Task, "fmt", "flake.nix"),
        );
        assert_eq!(record.effect, EffectClass::LocalBuild);
        let named = operation_record(
            &workspace("nixplane"),
            &Declaration::named(OperationKind::Task, "hub-update", "flake.nix"),
        );
        assert_eq!(named.effect, EffectClass::Deploy);
    }

    #[test]
    fn explain_reports_rustup_ahead_of_the_nix_shell_without_compiling() {
        let record = sample("lattice", "rust-test-pkgs");
        let report = explain(&record, "/Users/will/.cargo/bin:/nix/store/rust/bin");
        assert_eq!(report.compiler, "rustup");
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].field, "compiler");
        assert_eq!(report.rustc_wrapper.as_deref(), Some("sccache"));
        assert!(report.secret_refs.is_empty());
        let nix = explain(&record, "/nix/store/rust/bin:/usr/bin");
        assert!(nix.mismatches.is_empty());
        assert_eq!(nix.compiler, "nix");
    }
}
