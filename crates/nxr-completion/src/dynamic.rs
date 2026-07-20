//! Dynamic completion candidate protocol.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::ValueEnum;
use nxr_core::App;

use crate::cache::{DiscoveryCacheOptions, DiscoveryContext, cached_apps, discover_with_cache};

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
}

/// Discover app candidates for shell completion.
///
/// Uses the discovery cache when possible. On a cache miss, discovery runs in a
/// background thread and is abandoned after [`DISCOVERY_TIMEOUT`], returning an
/// empty list so shells never block on slow Nix evaluation.
pub fn discover_app_candidates<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Vec<App>
where
    F: FnOnce() -> Result<Vec<App>, E> + Send + 'static,
    E: Send + 'static,
{
    if options.refresh {
        return discover_with_timeout(context, options, discover);
    }

    if let Some(apps) = cached_apps(context) {
        return apps;
    }

    discover_with_timeout(context, options, discover)
}

fn discover_with_timeout<F, E>(
    context: &DiscoveryContext,
    options: DiscoveryCacheOptions,
    discover: F,
) -> Vec<App>
where
    F: FnOnce() -> Result<Vec<App>, E> + Send + 'static,
    E: Send + 'static,
{
    let context = context.clone();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let result = discover_with_cache(&context, options, discover);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(DISCOVERY_TIMEOUT) {
        Ok(Ok(apps)) => apps,
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
    use crate::cache::{DiscoveryCacheOptions, DiscoveryContext};
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
                description: Some("Run the linter".to_owned()),
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

        let mut buffer = Cursor::new(Vec::new());
        write_app_candidates(&apps, &mut buffer).expect("write candidates");
        let output = String::from_utf8(buffer.into_inner()).expect("utf-8 output");
        assert_eq!(output, "lint\tRun the linter\ntest\n");
    }

    #[test]
    fn discover_app_candidates_returns_empty_on_slow_discover() {
        let context = DiscoveryContext::new("github:owner/repo", None, "aarch64-darwin");

        let apps = discover_app_candidates(&context, DiscoveryCacheOptions::normal(), || {
            thread::sleep(DISCOVERY_TIMEOUT + Duration::from_millis(200));
            Ok::<_, std::convert::Infallible>(Vec::new())
        });

        assert!(apps.is_empty());
    }
}
