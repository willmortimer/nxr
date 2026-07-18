//! Resolve a discovered app by exact name.

use std::fmt;

use nxr_core::App;
use nxr_core::diagnostics::exit;

use crate::suggest::{DEFAULT_SUGGESTION_LIMIT, rank_app_suggestions};

/// No discovered app matches the requested name.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AppNotFoundError {
    /// Requested app name.
    pub name: String,
    /// Closest discovered app names for stderr hints.
    pub suggestions: Vec<String>,
}

impl fmt::Display for AppNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "app not found: {}", self.name)?;
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

impl std::error::Error for AppNotFoundError {}

impl AppNotFoundError {
    /// Stable `nxr` exit code for a missing app.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        exit::NOT_FOUND
    }
}

/// Resolve `name` against discovered apps using exact string equality.
///
/// # Errors
///
/// Returns [`AppNotFoundError`] when no app has `name`.
pub fn resolve_app_by_name<'apps>(
    apps: &'apps [App],
    name: &str,
) -> Result<&'apps App, AppNotFoundError> {
    apps.iter().find(|app| app.name == name).ok_or_else(|| {
        let suggestions = rank_app_suggestions(name, apps, DEFAULT_SUGGESTION_LIMIT)
            .into_iter()
            .map(str::to_owned)
            .collect();
        AppNotFoundError {
            name: name.to_owned(),
            suggestions,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::resolve_app_by_name;
    use nxr_core::App;
    use nxr_core::diagnostics::exit;

    fn sample_apps() -> Vec<App> {
        vec![
            App {
                name: "default".to_owned(),
                attr_path: "apps.aarch64-darwin.default".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: true,
                metadata: BTreeMap::new(),
            },
            App {
                name: "hello".to_owned(),
                attr_path: "apps.aarch64-darwin.hello".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("say hello".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ]
    }

    #[test]
    fn resolve_app_by_name_returns_exact_match() {
        let apps = sample_apps();
        let app = resolve_app_by_name(&apps, "hello").expect("hello app");
        assert_eq!(app.name, "hello");
        assert_eq!(app.attr_path, "apps.aarch64-darwin.hello");
    }

    #[test]
    fn resolve_app_by_name_rejects_unknown_name() {
        let apps = sample_apps();
        let error = resolve_app_by_name(&apps, "missing").expect_err("missing app");
        assert_eq!(error.name, "missing");
        assert!(error.suggestions.is_empty());
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn resolve_app_by_name_requires_exact_match() {
        let apps = sample_apps();
        let error = resolve_app_by_name(&apps, "Hello").expect_err("case mismatch");
        assert_eq!(error.name, "Hello");
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn resolve_app_by_name_includes_suggestions_for_close_miss() {
        let apps = sample_apps();
        let error = resolve_app_by_name(&apps, "helo").expect_err("typo");
        assert_eq!(error.name, "helo");
        assert_eq!(error.suggestions, vec!["hello".to_owned()]);
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }
}
