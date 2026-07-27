//! Task target selectors (`category:`, `changed`, `task:`, bare names).
//!
//! Used by `nxr task`, `nxr plan`, and `nxr list` to expand ergonomic selectors
//! into canonical task roots or affected-mode execution.

use std::fmt;

use crate::resolve::{listable_tasks, resolve_task_name};
use crate::schema::TaskDocument;

/// Prefix for category selectors (`category:<name>`).
pub const CATEGORY_PREFIX: &str = "category:";
/// Prefix for explicit task selectors (`task:<name>`).
pub const TASK_PREFIX: &str = "task:";
/// Prefix for explicit app selectors (`app:<name>`); not expanded to tasks here.
pub const APP_PREFIX: &str = "app:";
/// Selector token requesting affected-style roots (`changed`).
pub const CHANGED_SELECTOR: &str = "changed";

/// Parsed selector token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedSelector {
    /// `category:<name>`
    Category(String),
    /// `changed` (affected-style selection; requires path sources at the CLI layer).
    Changed,
    /// `task:<name>`
    Task(String),
    /// `app:<name>` (rejected for task expansion).
    App(String),
    /// Bare task name or alias.
    Bare(String),
}

/// Errors while parsing or expanding selectors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectorError {
    /// `app:` selectors cannot be expanded to task roots.
    AppSelector { name: String },
    /// No listable tasks match `category:<name>`.
    EmptyCategory { category: String },
    /// Token is not a known selector or task name.
    Unknown { token: String },
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppSelector { name } => {
                write!(
                    f,
                    "app selector `{APP_PREFIX}{name}` cannot be expanded to task roots; use `nxr run {name}`"
                )
            }
            Self::EmptyCategory { category } => {
                write!(f, "no listable tasks in category `{category}`")
            }
            Self::Unknown { token } => write!(f, "unknown selector or task: {token}"),
        }
    }
}

impl std::error::Error for SelectorError {}

/// Resolved task targets for `nxr task` / `nxr plan`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskTargetResolution {
    /// Canonical task roots (union when multiple selectors match).
    pub tasks: Vec<String>,
    /// When true, CLI should run affected analysis (same semantics as `--affected`).
    pub use_affected: bool,
}

/// Parse one selector token.
#[must_use]
pub fn parse_selector(token: &str) -> ParsedSelector {
    if token == CHANGED_SELECTOR {
        return ParsedSelector::Changed;
    }
    if let Some(category) = token.strip_prefix(CATEGORY_PREFIX) {
        return ParsedSelector::Category(category.to_owned());
    }
    if let Some(name) = token.strip_prefix(TASK_PREFIX) {
        return ParsedSelector::Task(name.to_owned());
    }
    if let Some(name) = token.strip_prefix(APP_PREFIX) {
        return ParsedSelector::App(name.to_owned());
    }
    ParsedSelector::Bare(token.to_owned())
}

/// Expand selector tokens against a task document.
///
/// `changed` sets [`TaskTargetResolution::use_affected`]. Category selectors add
/// every listable task in that category. Bare names and `task:` names resolve
/// through aliases.
///
/// # Errors
///
/// Returns [`SelectorError`] when a selector cannot be expanded.
///
/// # Panics
///
/// Panics only if `parse_selector` reports [`ParsedSelector::App`] for a token
/// that does not start with [`APP_PREFIX`] (internal invariant).
pub fn resolve_task_targets(
    doc: &TaskDocument,
    tokens: &[String],
) -> Result<TaskTargetResolution, SelectorError> {
    if tokens.is_empty() {
        return Ok(TaskTargetResolution {
            tasks: Vec::new(),
            use_affected: false,
        });
    }

    let mut tasks = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut use_affected = false;

    for token in tokens {
        match parse_selector(token) {
            ParsedSelector::Changed => use_affected = true,
            ParsedSelector::App { .. } => {
                let name = token
                    .strip_prefix(APP_PREFIX)
                    .expect("app prefix checked in parse_selector");
                return Err(SelectorError::AppSelector {
                    name: name.to_owned(),
                });
            }
            ParsedSelector::Category(category) => {
                let matched = listable_tasks(doc, Some(category.as_str()));
                if matched.is_empty() {
                    return Err(SelectorError::EmptyCategory { category });
                }
                for name in matched.keys() {
                    if seen.insert(name.clone()) {
                        tasks.push(name.clone());
                    }
                }
            }
            ParsedSelector::Task(name) | ParsedSelector::Bare(name) => {
                let canonical =
                    resolve_task_name(doc, &name).map_err(|_| SelectorError::Unknown {
                        token: token.clone(),
                    })?;
                let canonical = canonical.to_owned();
                if seen.insert(canonical.clone()) {
                    tasks.push(canonical);
                }
            }
        }
    }

    Ok(TaskTargetResolution {
        tasks,
        use_affected,
    })
}

/// List-view selector resolution (`nxr list [KIND_OR_SELECTOR]`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListSelectorResolution {
    pub category: Option<String>,
    pub affected_only: bool,
}

/// Parse a list filter token that is not a [`ListKind`] name.
///
/// # Errors
///
/// Returns [`SelectorError`] for unknown tokens.
pub fn resolve_list_selector(token: &str) -> Result<ListSelectorResolution, SelectorError> {
    match parse_selector(token) {
        ParsedSelector::Category(category) => Ok(ListSelectorResolution {
            category: Some(category),
            affected_only: false,
        }),
        ParsedSelector::Changed => Ok(ListSelectorResolution {
            category: None,
            affected_only: true,
        }),
        ParsedSelector::App(_) | ParsedSelector::Task(_) | ParsedSelector::Bare(_) => {
            Err(SelectorError::Unknown {
                token: token.to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::schema::{TaskDefinition, TaskDocument};

    fn sample_doc() -> TaskDocument {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), TaskDefinition::new("fmt"));
        let mut ci = TaskDefinition::new("ci");
        ci.category = Some("validation".to_owned());
        tasks.insert("ci".to_owned(), ci);
        TaskDocument::new(tasks)
    }

    #[test]
    fn parse_category_and_changed_selectors() {
        assert_eq!(
            parse_selector("category:validation"),
            ParsedSelector::Category("validation".to_owned())
        );
        assert_eq!(parse_selector("changed"), ParsedSelector::Changed);
        assert_eq!(
            parse_selector("task:ci"),
            ParsedSelector::Task("ci".to_owned())
        );
    }

    #[test]
    fn resolve_category_expands_listable_tasks() {
        let doc = sample_doc();
        let resolved =
            resolve_task_targets(&doc, &["category:validation".to_owned()]).expect("category");
        assert_eq!(resolved.tasks, vec!["ci".to_owned()]);
        assert!(!resolved.use_affected);
    }

    #[test]
    fn resolve_changed_sets_affected_flag() {
        let doc = sample_doc();
        let resolved =
            resolve_task_targets(&doc, &["changed".to_owned()]).expect("changed selector");
        assert!(resolved.tasks.is_empty());
        assert!(resolved.use_affected);
    }

    #[test]
    fn resolve_rejects_app_selector_for_tasks() {
        let doc = sample_doc();
        let err = resolve_task_targets(&doc, &["app:fmt".to_owned()]).expect_err("app");
        assert!(matches!(err, SelectorError::AppSelector { .. }));
    }
}
