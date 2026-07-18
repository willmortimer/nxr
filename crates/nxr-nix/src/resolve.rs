//! Resolve a discovered app by exact name.

use nxr_core::App;
use nxr_core::diagnostics::exit;

/// No discovered app matches the requested name.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[error("app not found: {name}")]
pub struct AppNotFoundError {
    /// Requested app name.
    pub name: String,
}

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
    apps.iter()
        .find(|app| app.name == name)
        .ok_or(AppNotFoundError {
            name: name.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{AppNotFoundError, resolve_app_by_name};
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
        assert_eq!(
            error,
            AppNotFoundError {
                name: "missing".to_owned(),
            }
        );
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }

    #[test]
    fn resolve_app_by_name_requires_exact_match() {
        let apps = sample_apps();
        let error = resolve_app_by_name(&apps, "Hello").expect_err("case mismatch");
        assert_eq!(error.name, "Hello");
        assert_eq!(error.exit_code(), exit::NOT_FOUND);
    }
}
