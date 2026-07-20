//! Git diff helpers for collecting changed paths.

use std::io;
use std::process::Command;

use camino::Utf8Path;
use thiserror::Error;

/// Errors while running `git diff` for changed paths.
#[derive(Debug, Error)]
pub enum GitDiffError {
    /// `git` is not available on `PATH`.
    #[error("git executable not found on PATH")]
    GitNotFound,
    /// `git diff` failed.
    #[error("git diff failed: {stderr}")]
    CommandFailed {
        /// Captured stderr from git.
        stderr: String,
    },
    /// Git output was not valid UTF-8.
    #[error("git diff output was not valid UTF-8")]
    InvalidUtf8,
}

/// Collect repository-relative paths changed between `base` and `HEAD`.
///
/// Uses `git diff --name-only --relative <base>...HEAD` from `repo_root`.
///
/// # Errors
///
/// Returns [`GitDiffError`] when git is missing or the diff command fails.
pub fn git_diff_name_only(repo_root: &Utf8Path, base: &str) -> Result<Vec<String>, GitDiffError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root.as_str())
        .args(["diff", "--name-only", "--relative"])
        .arg(format!("{base}...HEAD"))
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                GitDiffError::GitNotFound
            } else {
                GitDiffError::CommandFailed {
                    stderr: error.to_string(),
                }
            }
        })?;

    if !output.status.success() {
        return Err(GitDiffError::CommandFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let stdout = std::str::from_utf8(&output.stdout).map_err(|_| GitDiffError::InvalidUtf8)?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}
