//! Best-effort Justfile → nxr Nix migration.

use camino::Utf8Path;

use super::MigrateError;
use super::emit::{MigratedEntry, render_per_system_fragment};

pub const LIMITATIONS: &[&str] = &[
    "only simple `recipe:` and `recipe dep:` forms at column 0 are parsed",
    "recipe attributes, modules, imports, and expressions are ignored",
    "shebang and multi-line bodies are copied verbatim without validation",
    "no automatic runtimeInputs inference — add packages manually",
];

/// Parse a Justfile and return migrated entries.
pub fn parse_justfile_entries(contents: &str) -> Vec<MigratedEntry> {
    let recipes = parse_justfile(contents);
    recipes
        .into_iter()
        .map(|recipe| {
            let name = recipe.name.clone();
            MigratedEntry {
                name: recipe.name,
                description: format!("Migrated from justfile recipe `{name}`"),
                script: recipe.body,
                depends_on: recipe.depends_on,
            }
        })
        .collect()
}

/// Parse a Justfile and render a `perSystem` nxr fragment.
///
/// # Errors
///
/// Returns [`MigrateError`] when the file cannot be read or no recipes are found.
#[allow(dead_code)]
pub fn migrate_justfile(
    path: &Utf8Path,
    contents: &str,
    options: &super::emit::MigrateEmitOptions,
) -> Result<String, MigrateError> {
    let entries = parse_justfile_entries(contents);
    if entries.is_empty() {
        return Err(MigrateError::NoEntries {
            path: path.to_string(),
        });
    }

    Ok(render_per_system_fragment(
        &format!("justfile ({path})"),
        LIMITATIONS,
        &entries,
        options,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct JustRecipe {
    name: String,
    depends_on: Vec<String>,
    body: String,
}

fn parse_justfile(contents: &str) -> Vec<JustRecipe> {
    let mut recipes = Vec::new();
    let mut current: Option<JustRecipe> = None;

    for line in contents.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }

        if let Some(recipe) = parse_recipe_header(line) {
            if let Some(finished) = current.take() {
                recipes.push(finished);
            }
            current = Some(recipe);
            continue;
        }

        if let Some(recipe) = current.as_mut()
            && (line.starts_with('\t') || line.starts_with("    "))
        {
            let body_line = line
                .strip_prefix('\t')
                .or_else(|| line.strip_prefix("    "));
            if let Some(body_line) = body_line {
                if !recipe.body.is_empty() {
                    recipe.body.push('\n');
                }
                recipe.body.push_str(body_line);
            }
        }
    }

    if let Some(finished) = current {
        recipes.push(finished);
    }

    recipes
}

fn parse_recipe_header(line: &str) -> Option<JustRecipe> {
    let trimmed = line.trim_end();
    let (name_part, deps_part) = trimmed.split_once(':')?;
    if name_part.contains(' ') {
        return None;
    }
    let name = name_part.trim();
    if name.is_empty() || name.starts_with('@') {
        return None;
    }
    let depends_on = deps_part
        .split_whitespace()
        .filter(|dep| !dep.is_empty())
        .map(str::to_owned)
        .collect();
    Some(JustRecipe {
        name: name.to_owned(),
        depends_on,
        body: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::migrate_justfile;

    #[test]
    fn parses_dependencies_and_body() {
        let source = r#"
# setup
build:
    cargo build

test: build
    cargo test
"#;
        let rendered = migrate_justfile(Utf8Path::new("Justfile"), source, &Default::default())
            .expect("render");
        assert!(rendered.contains("build = {"));
        assert!(rendered.contains("test = {"));
        assert!(rendered.contains("cargo test"));
        assert!(rendered.contains("dependsOn = [ \"build\" ];"));
    }
}
