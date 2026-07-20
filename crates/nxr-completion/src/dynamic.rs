//! Dynamic completion candidate protocol.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::ValueEnum;
use nxr_core::App;

use crate::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};

/// Maximum time to wait for a cold discovery during interactive completion.
///
/// When discovery exceeds this budget, completion falls back to static command
/// names only (empty app candidates).
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Dynamic completion targets invoked through the hidden `__complete` command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CompleteTarget {
    /// Flake app names for the current workspace.
    Apps,
    /// Task names (and aliases) from optional `nxr` metadata.
    Tasks,
    /// `packages.<system>.*` leaf names.
    Packages,
    /// `checks.<system>.*` leaf names.
    Checks,
    /// `devShells.<system>.*` leaf names.
    Shells,
    /// Project namespaces from `nxr.projects.json`.
    Namespaces,
    /// Categories declared on apps/tasks.
    Categories,
}

/// Discover app candidates for shell completion.
///
/// Uses the discovery cache when possible. Cold misses should evaluate apps
/// and, when available, the lightweight `nxr` task document so `discoveryInputs`
/// enter the first cache entry. Task discovery remains best-effort so optional
/// metadata cannot erase ordinary app completions. On a cache miss, discovery
/// runs in a background thread and is abandoned after [`DISCOVERY_TIMEOUT`],
/// returning an empty list so shells never block on slow Nix evaluation.
pub fn discover_app_candidates<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Vec<App>
where
    F: FnOnce() -> Result<WorkspaceDiscovery, E> + Send + 'static,
    E: Send + 'static,
{
    // Cache hits return quickly inside discover_workspace_with_cache; cold
    // evaluation is abandoned after DISCOVERY_TIMEOUT.
    let context = context.clone();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let result = discover_workspace_with_cache(&context, options, discover);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(DISCOVERY_TIMEOUT) {
        Ok(Ok(workspace)) => workspace.apps,
        _ => Vec::new(),
    }
}

/// Write one candidate per line as `name` or `name<TAB>description`.
///
/// # Errors
///
/// Returns an I/O error when writing fails.
pub fn write_app_candidates(apps: &[App], writer: &mut dyn Write) -> io::Result<()> {
    for app in apps {
        match &app.description {
            Some(description) => writeln!(writer, "{}\t{description}", app.name)?,
            None => writeln!(writer, "{}", app.name)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::thread;
    use std::time::Duration;

    use clap::ValueEnum;

    use super::{CompleteTarget, DISCOVERY_TIMEOUT, discover_app_candidates, write_app_candidates};
    use crate::cache::{DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery};
    use nxr_core::App;

    #[test]
    fn complete_target_variants_include_apps() {
        assert_eq!(CompleteTarget::value_variants(), &[CompleteTarget::Apps]);
    }

    #[test]
    fn discovery_timeout_is_short() {
        assert!(DISCOVERY_TIMEOUT <= Duration::from_secs(1));
    }

    #[test]
    fn write_app_candidates_uses_tab_separated_descriptions() {
        let apps = vec![
            App {
                name: "lint".to_owned(),
                attr_path: "apps.aarch64-darwin.lint".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Run static analysis".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "test".to_owned(),
                attr_path: "apps.aarch64-darwin.test".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ];

        let mut cursor = Cursor::new(Vec::new());
        write_app_candidates(&apps, &mut cursor).expect("write");
        let output = String::from_utf8(cursor.into_inner()).expect("utf8");
        assert!(output.contains("lint\tRun static analysis\n"));
        assert!(output.contains("test\n"));
    }

    #[test]
    fn discover_app_candidates_returns_empty_on_slow_discover() {
        let context = DiscoveryContext::new("github:owner/repo", None, "aarch64-darwin");
        let apps = discover_app_candidates(&context, DiscoveryCacheOptions::normal(), || {
            thread::sleep(DISCOVERY_TIMEOUT + Duration::from_millis(200));
            Ok::<WorkspaceDiscovery, ()>(WorkspaceDiscovery {
                apps: Vec::new(),
                tasks: None,
            })
        });
        assert!(apps.is_empty());
    }
}
