//! Git / source identity for plan and store-exe cache invalidation.
//!
//! Store-exe reuse must track flake *source* that Nix would copy into
//! derivations — not only `.nix` / `flake.lock`. See ADR-0153.

use std::io;
use std::process::Command;

use camino::Utf8Path;

use crate::cas::digest_bytes;

/// Git HEAD + pathspec-scoped porcelain digest for a local flake root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitSourceIdentity {
    /// BLAKE3 hex of `HEAD:porcelain_digest` (porcelain scoped to `root`).
    pub digest: String,
    /// True when `git status --porcelain -- .` is non-empty under `root`.
    pub dirty: bool,
}

/// Compute git source identity for `flake_root`.
///
/// Returns `Ok(None)` when git is missing, the path is not in a repository, or
/// commands fail. Porcelain is limited to the flake root tree so sibling dirty
/// files in a monorepo do not spuriously invalidate.
///
/// # Errors
///
/// Returns I/O errors only when spawning git fails unexpectedly; missing git /
/// non-repo cases return `Ok(None)`.
pub fn git_source_identity(flake_root: &Utf8Path) -> io::Result<Option<GitSourceIdentity>> {
    let head = Command::new("git")
        .args(["-C", flake_root.as_str(), "rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() {
        return Ok(None);
    }
    let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head_sha.is_empty() {
        return Ok(None);
    }

    let status = Command::new("git")
        .args([
            "-C",
            flake_root.as_str(),
            "status",
            "--porcelain",
            "--",
            ".",
        ])
        .output()?;
    if !status.status.success() {
        return Ok(None);
    }
    let dirty = !status.stdout.is_empty();
    let dirty_digest = digest_bytes(&status.stdout);
    Ok(Some(GitSourceIdentity {
        digest: digest_bytes(format!("{head_sha}:{dirty_digest}").as_bytes()),
        dirty,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    use tempfile::TempDir;

    #[test]
    fn clean_and_dirty_git_identity() {
        let dir = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(dir.path()).expect("utf8");
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "test"]);
        fs::write(root.join("flake.nix"), "{}\n").expect("write");
        run_git(root, &["add", "flake.nix"]);
        run_git(root, &["commit", "-m", "init"]);

        let clean = git_source_identity(root).expect("git").expect("identity");
        assert!(!clean.dirty);

        fs::write(root.join("extra.txt"), "x\n").expect("dirty");
        let dirty = git_source_identity(root).expect("git").expect("identity");
        assert!(dirty.dirty);
        assert_ne!(clean.digest, dirty.digest);
    }

    fn run_git(root: &Utf8Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-C",
                root.as_str(),
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
}
