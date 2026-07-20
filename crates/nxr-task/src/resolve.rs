//! Task name resolution (canonical keys and aliases).
//!
//! Explicit commands (`nxr task`, `nxr graph`, `nxr inspect task`, `nxr watch`,
//! `nxr plan` when the name is not an app) resolve aliases to canonical task
//! names. Bare `nxr <name>` remains **app-only** and does not consult tasks.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::schema::{TaskDefinition, TaskDocument};

/// No task matches the requested name or alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveTaskError {
    pub name: String,
    pub suggestions: Vec<String>,
}

impl fmt::Display for ResolveTaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task not found: {}", self.name)?;
        if self.suggestions.is_empty() {
            return Ok(());
        }

        writeln!(f)?;
        writeln!(f)?;
        writeln!(f, "Did you mean:")?;
        for suggestion in &self.suggestions {
            writeln!(f, "  {suggestion}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ResolveTaskError {}

/// Resolve `name` to a canonical task id in `doc`.
///
/// Matches the task map key first, then unique aliases. Ambiguous aliases
/// (claimed by more than one task) are errors.
///
/// # Errors
///
/// Returns [`ResolveTaskError`] when no task matches or an alias is ambiguous.
pub fn resolve_task_name<'a>(
    doc: &'a TaskDocument,
    name: &str,
) -> Result<&'a str, ResolveTaskError> {
    if let Some((canonical, _)) = doc.tasks.get_key_value(name) {
        return Ok(canonical.as_str());
    }

    let mut owners: Vec<&'a str> = Vec::new();
    for (canonical, task) in &doc.tasks {
        if task.aliases.iter().any(|alias| alias == name) {
            owners.push(canonical.as_str());
        }
    }

    match owners.len() {
        0 => Err(ResolveTaskError {
            name: name.to_owned(),
            suggestions: rank_task_suggestions(name, doc),
        }),
        1 => Ok(owners[0]),
        _ => Err(ResolveTaskError {
            name: name.to_owned(),
            suggestions: owners.iter().map(|owner| (*owner).to_owned()).collect(),
        }),
    }
}

/// Resolve `name` and return the canonical id with its definition.
///
/// # Errors
///
/// Returns [`ResolveTaskError`] when resolution fails.
pub fn resolve_task<'a>(
    doc: &'a TaskDocument,
    name: &str,
) -> Result<(&'a str, &'a TaskDefinition), ResolveTaskError> {
    let canonical = resolve_task_name(doc, name)?;
    let task = doc.tasks.get(canonical).ok_or_else(|| ResolveTaskError {
        name: name.to_owned(),
        suggestions: Vec::new(),
    })?;
    Ok((canonical, task))
}

/// Tasks suitable for default listings: omit `hidden`, optionally filter
/// `category` and/or namespace membership.
#[must_use]
pub fn listable_tasks(
    doc: &TaskDocument,
    category: Option<&str>,
) -> BTreeMap<String, TaskDefinition> {
    listable_tasks_filtered(doc, category, None)
}

/// Like [`listable_tasks`], with an optional namespace membership set.
#[must_use]
pub fn listable_tasks_filtered(
    doc: &TaskDocument,
    category: Option<&str>,
    namespace_tasks: Option<&BTreeSet<String>>,
) -> BTreeMap<String, TaskDefinition> {
    doc.tasks
        .iter()
        .filter(|(_, task)| !task.hidden)
        .filter(|(_, task)| category.is_none_or(|cat| task.category.as_deref() == Some(cat)))
        .filter(|(name, _)| namespace_tasks.is_none_or(|members| members.contains(*name)))
        .map(|(name, task)| (name.clone(), task.clone()))
        .collect()
}

/// Copy listing metadata from `doc.apps` onto discovered flake apps.
///
/// Sets [`nxr_core::NXR_CATEGORY_KEY`] when the document provides a category.
/// Does not invent apps: only enriches names already discovered from the flake.
pub fn enrich_apps_with_listing_metadata(apps: &mut [nxr_core::App], doc: &TaskDocument) {
    for app in apps {
        if let Some(meta) = doc.apps.get(&app.name) {
            nxr_core::set_app_category(app, meta.category.as_deref());
        }
    }
}

fn rank_task_suggestions(query: &str, doc: &TaskDocument) -> Vec<String> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower = query.to_ascii_lowercase();
    let mut scored: Vec<(u32, String)> = doc
        .tasks
        .keys()
        .chain(doc.tasks.values().flat_map(|task| task.aliases.iter()))
        .filter_map(|name| {
            let name_lower = name.to_ascii_lowercase();
            let score = if name_lower.starts_with(&query_lower) {
                0
            } else if name_lower.contains(&query_lower) {
                1
            } else {
                return None;
            };
            Some((score, name.clone()))
        })
        .collect();

    scored.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    scored.sort_by_key(|(score, _)| *score);

    let mut suggestions = Vec::new();
    for (_, name) in scored {
        if suggestions.len() >= 5 {
            break;
        }
        if !suggestions.contains(&name) {
            suggestions.push(name);
        }
    }
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TaskDefinition;

    fn sample_doc() -> TaskDocument {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), TaskDefinition::new("fmt"));
        let mut ci = TaskDefinition::new("ci");
        ci.depends_on = vec!["test".to_owned()];
        ci.category = Some("validation".to_owned());
        ci.aliases = vec!["check".to_owned(), "gate".to_owned()];
        tasks.insert("ci".to_owned(), ci);
        let mut hidden = TaskDefinition::new("secret");
        hidden.hidden = true;
        hidden.category = Some("validation".to_owned());
        tasks.insert("hidden-task".to_owned(), hidden);
        TaskDocument::new(tasks)
    }

    #[test]
    fn resolve_canonical_name() {
        let doc = sample_doc();
        assert_eq!(resolve_task_name(&doc, "ci").expect("ci"), "ci");
    }

    #[test]
    fn resolve_alias_to_canonical() {
        let doc = sample_doc();
        assert_eq!(resolve_task_name(&doc, "check").expect("alias"), "ci");
        assert_eq!(resolve_task_name(&doc, "gate").expect("alias"), "ci");
    }

    #[test]
    fn resolve_unknown_includes_suggestions() {
        let doc = sample_doc();
        let err = resolve_task_name(&doc, "c").expect_err("unknown");
        assert_eq!(err.name, "c");
        assert!(err.suggestions.contains(&"ci".to_owned()));
    }

    #[test]
    fn listable_tasks_omit_hidden_and_filter_category() {
        let doc = sample_doc();
        let visible = listable_tasks(&doc, None);
        assert_eq!(visible.len(), 2);
        assert!(visible.contains_key("fmt"));
        assert!(visible.contains_key("ci"));

        let validation = listable_tasks(&doc, Some("validation"));
        assert_eq!(validation.len(), 1);
        assert!(validation.contains_key("ci"));
    }
}
