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
/// Store-exe reuse and `includeGitState` action keys use this full status.
/// Prepared plans use [`git_plan_source_identity`], which only asks Git about
/// `.nix` files, `flake.lock`, and declared discovery inputs.
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

/// Plan-cache source identity limited to Nix inputs.
///
/// `discovery_inputs` are repository-relative paths. Untracked `.nix` bytes are
/// also covered by the nix-tree fingerprint; this status still sees additions
/// and edits of the pathspec so the plan key changes with them. A dirty
/// unrelated file does not. Returns `Ok(None)` in the same cases as
/// [`git_source_identity`].
///
/// # Errors
///
/// Returns an I/O error when spawning git fails unexpectedly.
pub fn git_plan_source_identity(
    flake_root: &Utf8Path,
    discovery_inputs: &[String],
) -> io::Result<Option<GitSourceIdentity>> {
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

    let mut args = vec![
        "-C".to_owned(),
        flake_root.as_str().to_owned(),
        "status".to_owned(),
        "--porcelain=v1".to_owned(),
        "-z".to_owned(),
        "--untracked-files=all".to_owned(),
        "--".to_owned(),
        ":(glob)*.nix".to_owned(),
        ":(glob)**/*.nix".to_owned(),
        ":(literal)flake.lock".to_owned(),
    ];
    for path in discovery_inputs {
        if path.is_empty() {
            continue;
        }
        args.push(format!(":(literal){path}"));
    }
    let status = Command::new("git").args(&args).output()?;
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

        let plan = git_plan_source_identity(root, &[])
            .expect("plan")
            .expect("plan identity");
        assert!(!plan.dirty, "unrelated files stay out of the plan identity");
        assert_ne!(plan.digest, dirty.digest);

        fs::create_dir_all(root.join("nix")).expect("mkdir");
        fs::write(root.join("nix/app.nix"), "{}\n").expect("nix");
        let nix_dirty = git_plan_source_identity(root, &[])
            .expect("plan")
            .expect("nix identity");
        assert!(nix_dirty.dirty);
        assert_ne!(plan.digest, nix_dirty.digest);

        fs::write(root.join("notes.txt"), "n\n").expect("notes");
        let notes_ignored = git_plan_source_identity(root, &[])
            .expect("plan")
            .expect("notes ignored");
        assert_eq!(notes_ignored.digest, nix_dirty.digest);
        let notes_declared = git_plan_source_identity(root, &["notes.txt".to_owned()])
            .expect("plan")
            .expect("notes declared");
        assert!(notes_declared.dirty);
        assert_ne!(notes_declared.digest, nix_dirty.digest);
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
