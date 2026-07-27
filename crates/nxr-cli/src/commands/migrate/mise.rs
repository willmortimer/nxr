//! Best-effort mise.toml → nxr Nix migration.

use std::collections::BTreeMap;

use camino::Utf8Path;
use serde::Deserialize;

use super::MigrateError;
use super::emit::{MigratedEntry, render_per_system_fragment};

const LIMITATIONS: &[&str] = &[
    "only `[tasks]` entries are migrated; tool pins and env blocks are ignored",
    "string tasks become single-line scripts; table tasks use the `run` field",
    "task `depends` become nxr `dependsOn` when names match other migrated tasks",
    "no automatic runtimeInputs inference — add packages manually",
];

/// Parse `mise.toml` and render a `perSystem` nxr fragment.
///
/// # Errors
///
/// Returns [`MigrateError`] when parsing fails or no tasks are found.
pub fn migrate_mise(path: &Utf8Path, contents: &str) -> Result<String, MigrateError> {
    let document: MiseDocument = toml::from_str(contents).map_err(|error| MigrateError::Parse {
        path: path.to_string(),
        message: error.to_string(),
    })?;
    let tasks = document.tasks.unwrap_or_default();
    if tasks.is_empty() {
        return Err(MigrateError::NoEntries {
            path: path.to_string(),
        });
    }

    let entries = tasks
        .into_iter()
        .map(|(name, task)| {
            let (script, depends_on) = match task {
                MiseTask::Run(command) => (command, Vec::new()),
                MiseTask::Table(table) => (
                    table.run.unwrap_or_default(),
                    table.depends.unwrap_or_default(),
                ),
            };
            MigratedEntry {
                name: name.clone(),
                description: format!("Migrated from mise task `{name}`"),
                script,
                depends_on,
            }
        })
        .collect::<Vec<_>>();

    Ok(render_per_system_fragment(
        &format!("mise.toml ({path})"),
        LIMITATIONS,
        &entries,
    ))
}

#[derive(Debug, Deserialize)]
struct MiseDocument {
    #[serde(default)]
    tasks: Option<BTreeMap<String, MiseTask>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MiseTask {
    Run(String),
    Table(MiseTaskTable),
}

#[derive(Debug, Deserialize)]
struct MiseTaskTable {
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    depends: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::migrate_mise;

    #[test]
    fn parses_string_and_table_tasks() {
        let source = r#"
[tasks.build]
run = "cargo build"

[tasks.test]
depends = ["build"]
run = "cargo test"

[tasks.fmt]
run = "cargo fmt --all"
"#;
        let rendered = migrate_mise(Utf8Path::new("mise.toml"), source).expect("render");
        assert!(rendered.contains("build = {"));
        assert!(rendered.contains("test = {"));
        assert!(rendered.contains("dependsOn = [ \"build\" ];"));
    }

    #[test]
    fn parses_inline_string_tasks() {
        let source = r#"
[tasks]
lint = "cargo clippy"
"#;
        let rendered = migrate_mise(Utf8Path::new("mise.toml"), source).expect("render");
        assert!(rendered.contains("lint = {"));
        assert!(rendered.contains("cargo clippy"));
    }
}
