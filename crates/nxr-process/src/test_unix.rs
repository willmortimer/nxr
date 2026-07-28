//! Shared Unix helpers for hermetic unit tests.
//!
//! Nix check sandboxes often omit `/usr/bin` and `/bin`; coreutils still land
//! on `PATH` via the stdenv.

use std::path::Path;

/// Resolve a Unix utility by absolute path, preferring FHS then `PATH`.
pub(crate) fn unix_util(name: &str) -> String {
    for prefix in ["/usr/bin", "/bin"] {
        let candidate = format!("{prefix}/{name}");
        if Path::new(&candidate).exists() {
            return candidate;
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    panic!("missing {name} under /usr/bin, /bin, or PATH");
}
