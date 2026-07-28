//! Shared helpers for nxr CLI integration tests.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Repository root (`nxr/`), two levels above `crates/nxr-cli`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Skip the current test when Nix integration is disabled or `nix` is not on
/// `PATH`.
///
/// Integration tests call Nix and are expected to run in CI and dev shells that
/// provide it. Local `cargo test` without Nix stays green via this soft skip.
pub fn require_nix() -> Option<()> {
    if std::env::var_os("NXR_SKIP_NIX_INTEGRATION").is_some() {
        eprintln!("skipping integration test: Nix integration is disabled");
        return None;
    }

    if which::which("nix").is_ok() {
        return Some(());
    }

    eprintln!("skipping integration test: `nix` not found on PATH");
    None
}

/// Classify a `nix` argv slice for call-budget logging.
///
/// Keep the bash wrapper in [`NixCallCounter::install`] aligned with this
/// function.
#[must_use]
pub fn classify_nix_argv(args: &[&str]) -> &'static str {
    if args.first() == Some(&"--version") {
        return "version";
    }
    if matches!(args, ["config", "show", ..]) || args.first() == Some(&"show-config") {
        return "config";
    }
    if args.contains(&"--help") {
        return "help";
    }
    match args.first().copied() {
        Some("flake") if args.get(1) == Some(&"show") => "flake-show",
        Some("run") => "run",
        Some("eval") => "eval",
        Some("build") => "build",
        Some("develop") => "develop",
        _ => "other",
    }
}

/// Counting shim around the real `nix` binary for call-budget tests.
pub struct NixCallCounter {
    _temp: TempDir,
    pub wrapper: PathBuf,
    pub log: PathBuf,
}

impl NixCallCounter {
    /// Install a wrapper that logs `flake-show` / `run` / `eval` / `develop` /
    /// capability-probe (`version`, `config`, `help`) lines.
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
  if [[ "${{1:-}}" == "--version" ]]; then
    echo "version"
  elif [[ "${{1:-}}" == "config" && "${{2:-}}" == "show" ]] || [[ "${{1:-}}" == "show-config" ]]; then
    echo "config"
  else
    found_help=0
    for arg in "$@"; do
      if [[ "$arg" == "--help" ]]; then
        found_help=1
        break
      fi
    done
    if [[ "$found_help" -eq 1 ]]; then
      echo "help"
    elif [[ "${{1:-}}" == "flake" && "${{2:-}}" == "show" ]]; then
      echo "flake-show"
    elif [[ "${{1:-}}" == "run" ]]; then
      echo "run"
    elif [[ "${{1:-}}" == "eval" ]]; then
      echo "eval"
    elif [[ "${{1:-}}" == "build" ]]; then
      echo "build"
    elif [[ "${{1:-}}" == "develop" ]]; then
      echo "develop"
    else
      echo "other"
    fi
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

#[cfg(test)]
mod tests {
    use super::classify_nix_argv;

    #[test]
    fn classify_nix_argv_distinguishes_capability_probes() {
        assert_eq!(classify_nix_argv(&["--version"]), "version");
        assert_eq!(classify_nix_argv(&["config", "show", "--json"]), "config");
        assert_eq!(classify_nix_argv(&["show-config", "--json"]), "config");
        assert_eq!(classify_nix_argv(&["--help"]), "help");
        assert_eq!(classify_nix_argv(&["flake", "--help"]), "help");
        assert_eq!(classify_nix_argv(&["eval", "--help"]), "help");
    }

    #[test]
    fn classify_nix_argv_keeps_primary_command_classes() {
        assert_eq!(
            classify_nix_argv(&["flake", "show", "--json"]),
            "flake-show"
        );
        assert_eq!(classify_nix_argv(&["run", ".#hello"]), "run");
        assert_eq!(classify_nix_argv(&["eval", "--json", ".#x"]), "eval");
        assert_eq!(
            classify_nix_argv(&["build", "--no-link", "--print-out-paths", "/nix/store/x"]),
            "build"
        );
        assert_eq!(classify_nix_argv(&["develop"]), "develop");
        assert_eq!(classify_nix_argv(&["store", "ping"]), "other");
    }
}
