//! Shared helpers for nxr CLI integration tests.

use std::path::{Path, PathBuf};

/// Repository root (`nxr/`), two levels above `crates/nxr-cli`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Skip the current test when `nix` is not on `PATH`.
///
/// Integration tests call Nix and are expected to run in CI and dev shells that
/// provide it. Local `cargo test` without Nix stays green via this soft skip.
pub fn require_nix() -> Option<()> {
    if which::which("nix").is_ok() {
        return Some(());
    }

    eprintln!("skipping integration test: `nix` not found on PATH");
    None
}
