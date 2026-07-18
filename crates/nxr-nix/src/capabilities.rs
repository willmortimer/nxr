//! Nix executable discovery and current-system detection.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::NixError;
use crate::command::{self, NIX_EXECUTABLE_ENV};

/// Whether a Nix failure should map to capability (4) vs evaluation (5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NixFailureKind {
    Capability,
    Evaluation,
}

/// Locate the `nix` executable via `NXR_NIX` or `PATH`.
///
/// # Errors
///
/// Returns [`NixError::NixNotFound`] when no usable `nix` executable is available.
pub fn locate_nix() -> Result<Utf8PathBuf, NixError> {
    if let Ok(explicit) = std::env::var(NIX_EXECUTABLE_ENV) {
        let path = Utf8PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(NixError::NixNotFound { path });
    }

    let path = which::which("nix").map_err(|_| NixError::NixNotFound {
        path: Utf8PathBuf::from("nix"),
    })?;

    Utf8PathBuf::from_path_buf(path).map_err(|_| NixError::NixNotFound {
        path: Utf8PathBuf::from("nix"),
    })
}

/// Detect the current Nix system string (for example `aarch64-darwin`).
///
/// # Errors
///
/// Returns [`NixError`] when `nix eval` fails or returns an empty system string.
pub fn detect_system(nix: &Utf8Path) -> Result<String, NixError> {
    let args = command::current_system_args();
    let output = run_nix(nix, &args, NixFailureKind::Capability)?;

    let system = String::from_utf8(output).map_err(|_| NixError::InvalidSystemOutput)?;
    let system = system.trim();
    if system.is_empty() {
        return Err(NixError::InvalidSystemOutput);
    }

    Ok(system.to_owned())
}

/// Run `nix` with `args` and return stdout on success.
///
/// # Errors
///
/// Returns [`NixError`] when `nix` cannot be spawned or exits unsuccessfully.
pub fn run_nix(
    nix: &Utf8Path,
    args: &[String],
    failure_kind: NixFailureKind,
) -> Result<Vec<u8>, NixError> {
    let output = Command::new(nix.as_std_path())
        .args(args)
        .output()
        .map_err(|source| NixError::SpawnFailed {
            nix: nix.to_path_buf(),
            source,
        })?;

    if output.status.success() {
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(NixError::CommandFailed {
        nix: nix.to_path_buf(),
        args: args.to_vec(),
        status: output.status.code(),
        stderr,
        kind: failure_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::{detect_system, locate_nix};

    #[test]
    fn locate_nix_finds_executable_on_path() {
        let nix = locate_nix().expect("nix should be on PATH in dev environments");
        assert!(nix.is_file());
    }

    #[test]
    #[ignore = "requires nix on PATH"]
    fn detect_system_returns_current_platform() {
        let nix = locate_nix().expect("nix");
        let system = detect_system(&nix).expect("detect system");
        assert!(!system.is_empty());
        assert!(system.contains('-'));
    }
}
