//! Shared helpers for nxr CLI integration tests.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

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

/// Counting shim around the real `nix` binary for call-budget tests.
pub struct NixCallCounter {
    _temp: TempDir,
    pub wrapper: PathBuf,
    pub log: PathBuf,
}

impl NixCallCounter {
    /// Install a wrapper that logs `flake-show` / `run` / `eval` / `develop` lines.
    pub fn install() -> Self {
        let real_nix = which::which("nix").expect("nix on PATH");
        let temp = TempDir::new().expect("tempdir");
        let log = temp.path().join("nix-calls.log");
        let wrapper = temp.path().join("nix");
        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
LOG={log}
REAL_NIX={real}
{{
  if [[ "${{1:-}}" == "flake" && "${{2:-}}" == "show" ]]; then
    echo "flake-show"
  elif [[ "${{1:-}}" == "run" ]]; then
    echo "run"
  elif [[ "${{1:-}}" == "eval" ]]; then
    echo "eval"
  elif [[ "${{1:-}}" == "develop" ]]; then
    echo "develop"
  else
    echo "other"
  fi
}} >> "$LOG"
exec "$REAL_NIX" "$@"
"#,
            log = sh_single_quote(&log),
            real = sh_single_quote(&real_nix),
        );
        fs::write(&wrapper, script).expect("write nix wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&wrapper).expect("meta").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&wrapper, perms).expect("chmod");
        }
        fs::write(&log, "").expect("init log");
        Self {
            _temp: temp,
            wrapper,
            log,
        }
    }

    pub fn count(&self, kind: &str) -> usize {
        let contents = fs::read_to_string(&self.log).unwrap_or_default();
        contents.lines().filter(|line| *line == kind).count()
    }
}

fn sh_single_quote(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}
