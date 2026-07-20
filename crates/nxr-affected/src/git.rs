//! Git diff helpers for collecting changed paths.

use std::collections::BTreeSet;
use std::io;
use std::process::Command;

use camino::Utf8Path;
use thiserror::Error;

/// Errors while running git for changed paths.
#[derive(Debug, Error)]
pub enum GitDiffError {
    /// `git` is not available on `PATH`.
    #[error("git executable not found on PATH")]
    GitNotFound,
    /// A git command failed.
    #[error("git command failed: {stderr}")]
    CommandFailed {
        /// Captured stderr from git.
        stderr: String,
    },
    /// Git output was not valid UTF-8.
    #[error("git output was not valid UTF-8")]
    InvalidUtf8,
}

/// Collect repository-relative paths changed between `base` and `HEAD`.
///
/// Uses `git diff --name-status -z --find-renames --relative <base>...HEAD`.
/// Rename and copy records contribute both the source and destination paths.
///
/// # Errors
///
/// Returns [`GitDiffError`] when git is missing or the diff command fails.
pub fn git_diff_name_only(repo_root: &Utf8Path, base: &str) -> Result<Vec<String>, GitDiffError> {
    git_name_status_paths(
        repo_root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--relative",
            &format!("{base}...HEAD"),
        ],
    )
}

/// Collect unstaged, staged, and untracked paths in the working tree.
///
/// # Errors
///
/// Returns [`GitDiffError`] when git is missing or a command fails.
pub fn git_working_tree_changes(repo_root: &Utf8Path) -> Result<Vec<String>, GitDiffError> {
    let mut paths = BTreeSet::new();
    for path in git_name_status_paths(
        repo_root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--relative",
        ],
    )? {
        paths.insert(path);
    }
    for path in git_name_status_paths(
        repo_root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--relative",
            "--cached",
        ],
    )? {
        paths.insert(path);
    }
    for path in git_nul_paths(
        repo_root,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )? {
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

/// Union of [`git_diff_name_only`] and [`git_working_tree_changes`].
///
/// # Errors
///
/// Returns [`GitDiffError`] when either collection fails.
pub fn git_all_changes(repo_root: &Utf8Path, base: &str) -> Result<Vec<String>, GitDiffError> {
    let mut paths = BTreeSet::new();
    for path in git_diff_name_only(repo_root, base)? {
        paths.insert(path);
    }
    for path in git_working_tree_changes(repo_root)? {
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

fn git_name_status_paths(repo_root: &Utf8Path, args: &[&str]) -> Result<Vec<String>, GitDiffError> {
    let stdout = git_stdout(repo_root, args)?;
    Ok(parse_name_status_z(&stdout))
}

fn git_nul_paths(repo_root: &Utf8Path, args: &[&str]) -> Result<Vec<String>, GitDiffError> {
    let stdout = git_stdout(repo_root, args)?;
    Ok(stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn git_stdout(repo_root: &Utf8Path, args: &[&str]) -> Result<String, GitDiffError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root.as_str())
        .args(args)
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

    String::from_utf8(output.stdout).map_err(|_| GitDiffError::InvalidUtf8)
}

/// Parse `git diff --name-status -z` output into repository-relative paths.
///
/// Rename (`R*`) and copy (`C*`) records contribute both old and new paths.
fn parse_name_status_z(stdout: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut parts = stdout.split('\0').filter(|part| !part.is_empty());
    while let Some(status) = parts.next() {
        let code = status.chars().next().unwrap_or('\0');
        match code {
            'R' | 'C' => {
                if let Some(old) = parts.next() {
                    paths.push(old.to_owned());
                }
                if let Some(new) = parts.next() {
                    paths.push(new.to_owned());
                }
            }
            _ => {
                if let Some(path) = parts.next() {
                    paths.push(path.to_owned());
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use camino::Utf8Path;
    use tempfile::TempDir;

    use super::{
        git_all_changes, git_diff_name_only, git_working_tree_changes, parse_name_status_z,
    };

    fn init_repo() -> TempDir {
        let temp = TempDir::new().expect("tempdir");
        run(temp.path(), &["git", "init", "-b", "main"]);
        run(
            temp.path(),
            &["git", "config", "user.email", "test@example.com"],
        );
        run(temp.path(), &["git", "config", "user.name", "nxr test"]);
        temp
    }

    fn run(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|error| panic!("spawn {}: {error}", args[0]));
        assert!(status.success(), "{args:?} failed in {}", cwd.display());
    }

    fn write(cwd: &std::path::Path, relative: &str, contents: &str) {
        let path = cwd.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write");
    }

    #[test]
    fn parse_name_status_includes_both_rename_paths() {
        let paths = parse_name_status_z("R100\0crates/api/foo\0crates/web/foo\0M\0README.md\0");
        assert_eq!(
            paths,
            vec![
                "crates/api/foo".to_owned(),
                "crates/web/foo".to_owned(),
                "README.md".to_owned()
            ]
        );
    }

    #[test]
    fn working_tree_includes_unstaged_staged_and_untracked() {
        let temp = init_repo();
        let root = Utf8Path::from_path(temp.path()).expect("utf8");
        write(temp.path(), "tracked.txt", "v1\n");
        run(temp.path(), &["git", "add", "tracked.txt"]);
        run(temp.path(), &["git", "commit", "-m", "initial"]);

        write(temp.path(), "tracked.txt", "v2\n");
        write(temp.path(), "staged.txt", "staged\n");
        run(temp.path(), &["git", "add", "staged.txt"]);
        write(temp.path(), "untracked.txt", "loose\n");

        let paths = git_working_tree_changes(root).expect("working tree");
        assert!(paths.iter().any(|p| p == "tracked.txt"));
        assert!(paths.iter().any(|p| p == "staged.txt"));
        assert!(paths.iter().any(|p| p == "untracked.txt"));
    }

    #[test]
    fn all_changes_unions_base_range_and_working_tree() {
        let temp = init_repo();
        let root = Utf8Path::from_path(temp.path()).expect("utf8");
        write(temp.path(), "base.txt", "base\n");
        run(temp.path(), &["git", "add", "base.txt"]);
        run(temp.path(), &["git", "commit", "-m", "base"]);
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(temp.path())
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned();

        write(temp.path(), "committed.txt", "committed\n");
        run(temp.path(), &["git", "add", "committed.txt"]);
        run(temp.path(), &["git", "commit", "-m", "on branch"]);

        write(temp.path(), "dirty.txt", "dirty\n");

        let range_only = git_diff_name_only(root, &base).expect("range");
        assert!(range_only.iter().any(|p| p == "committed.txt"));
        assert!(!range_only.iter().any(|p| p == "dirty.txt"));

        let all = git_all_changes(root, &base).expect("all changes");
        assert!(all.iter().any(|p| p == "committed.txt"));
        assert!(all.iter().any(|p| p == "dirty.txt"));
    }

    #[test]
    fn base_diff_includes_rename_source_and_destination() {
        let temp = init_repo();
        let root = Utf8Path::from_path(temp.path()).expect("utf8");
        write(temp.path(), "crates/api/foo.txt", "body\n");
        run(temp.path(), &["git", "add", "crates/api/foo.txt"]);
        run(temp.path(), &["git", "commit", "-m", "api"]);
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(temp.path())
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned();

        fs::create_dir_all(temp.path().join("crates/web")).expect("mkdir");
        run(
            temp.path(),
            &["git", "mv", "crates/api/foo.txt", "crates/web/foo.txt"],
        );
        run(temp.path(), &["git", "commit", "-m", "rename"]);

        let paths = git_diff_name_only(root, &base).expect("rename diff");
        assert!(paths.iter().any(|p| p == "crates/api/foo.txt"));
        assert!(paths.iter().any(|p| p == "crates/web/foo.txt"));
    }
}
