//! Cargo output locations that share one target or build directory.
//!
//! Nix derivations are not cargo locks. An unrecognized command is an error:
//! an empty list is only success for `nix` and `nix-build`.

use std::fs;
use std::path::{Path, PathBuf};

/// One directory a command writes, plus the cohort that writes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputResource {
    pub kind: String,
    pub path: PathBuf,
    pub cohort: String,
}

/// Resolve the output directories a command will write.
///
/// `cargo`, or any command that passes `--target-dir`, yields a `cargo_target`
/// resource. A non-empty `cargo_build_dir` that is not that same path also
/// yields a `cargo_build` resource. `nix` and `nix-build` yield an empty list.
///
/// # Errors
///
/// Returns an error containing `output location is unknown` when the command
/// is not a Nix invocation and does not name a cargo output directory.
pub fn resolve_output_resources(
    workspace_root: &Path,
    command: &[String],
    cargo_target_dir: Option<&str>,
    cargo_build_dir: Option<&str>,
    platform: &str,
    arch: &str,
    toolchain: &str,
) -> Result<Vec<OutputResource>, String> {
    if is_nix_invocation(command) {
        return Ok(Vec::new());
    }
    if !locks_cargo_output(command) {
        let program = command.first().map(String::as_str).unwrap_or("<empty>");
        return Err(format!("output location is unknown for {program}"));
    }

    let target_path = effective_target_dir(workspace_root, cargo_target_dir, command);
    let cohort = cohort_id(platform, arch, toolchain);
    let build_path = cargo_build_dir
        .filter(|value| !value.is_empty())
        .map(|value| rooted(workspace_root, value))
        .filter(|path| path != &target_path);

    let mut resources = Vec::with_capacity(1 + usize::from(build_path.is_some()));
    resources.push(output_resource("cargo_target", target_path, &cohort));
    if let Some(path) = build_path {
        resources.push(output_resource("cargo_build", path, &cohort));
    }
    Ok(resources)
}

fn output_resource(kind: &str, path: PathBuf, cohort: &str) -> OutputResource {
    OutputResource {
        kind: kind.to_owned(),
        path,
        cohort: cohort.to_owned(),
    }
}

/// Two resources conflict when they name the same directory.
///
/// Kind and cohort are not part of the identity. A symlink and its real path
/// conflict. A missing path is compared in absolute form.
#[must_use]
pub fn resources_conflict(left: &OutputResource, right: &OutputResource) -> bool {
    comparable_path(&left.path) == comparable_path(&right.path)
}

fn is_nix_invocation(command: &[String]) -> bool {
    matches!(command_name(command), Some("nix" | "nix-build"))
}

fn locks_cargo_output(command: &[String]) -> bool {
    command_name(command) == Some("cargo")
        || command
            .iter()
            .any(|arg| arg == "--target-dir" || arg.starts_with("--target-dir="))
}

fn command_name(command: &[String]) -> Option<&str> {
    let program = command.first()?;
    Path::new(program).file_name()?.to_str()
}

/// Same precedence as `target_lock::effective_target_dir`.
///
/// Duplicated here so this module does not depend on `target_lock`.
fn effective_target_dir(
    workspace_root: &Path,
    cargo_target_dir: Option<&str>,
    command: &[String],
) -> PathBuf {
    if let Some(from_command) = command_target_dir(command) {
        return rooted(workspace_root, from_command);
    }
    if let Some(from_env) = cargo_target_dir.filter(|value| !value.is_empty()) {
        return rooted(workspace_root, from_env);
    }
    workspace_root.join("target")
}

fn command_target_dir(command: &[String]) -> Option<&str> {
    let mut args = command.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--target-dir=")
            && !value.is_empty()
        {
            return Some(value);
        }
        if arg == "--target-dir"
            && let Some(value) = args.next()
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

fn rooted(workspace_root: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn cohort_id(platform: &str, arch: &str, toolchain: &str) -> String {
    let toolchain = toolchain
        .replace("://", "redacted")
        .replace('@', "redacted");
    format!("{platform}-{arch}-{toolchain}")
}

fn comparable_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| absolute_path(path))
}

fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().copied().map(str::to_owned).collect()
    }

    fn resource(kind: &str, path: PathBuf, cohort: &str) -> OutputResource {
        OutputResource {
            kind: kind.to_owned(),
            path,
            cohort: cohort.to_owned(),
        }
    }

    #[test]
    fn cargo_command_resolves_workspace_target_when_env_is_empty() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let resources = resolve_output_resources(
            workspace.path(),
            &argv(&["cargo", "build"]),
            None,
            None,
            "darwin",
            "aarch64",
            "stable",
        )
        .expect("cargo target");
        assert_eq!(
            resources,
            vec![resource(
                "cargo_target",
                workspace.path().join("target"),
                "darwin-aarch64-stable",
            )]
        );

        let empty_env = resolve_output_resources(
            workspace.path(),
            &argv(&["/usr/bin/cargo", "check"]),
            Some(""),
            Some(""),
            "darwin",
            "aarch64",
            "stable",
        )
        .expect("empty env still falls back");
        assert_eq!(empty_env, resources);

        let same_build_dir = resolve_output_resources(
            workspace.path(),
            &argv(&["cargo", "build"]),
            None,
            Some("target"),
            "darwin",
            "aarch64",
            "stable",
        )
        .expect("build dir equal to target");
        assert_eq!(same_build_dir, resources);

        let from_env = resolve_output_resources(
            workspace.path(),
            &argv(&["cargo", "build"]),
            Some("env-relative"),
            None,
            "darwin",
            "aarch64",
            "stable",
        )
        .expect("env target");
        assert_eq!(
            from_env,
            vec![resource(
                "cargo_target",
                workspace.path().join("env-relative"),
                "darwin-aarch64-stable",
            )]
        );

        let redacted = resolve_output_resources(
            workspace.path(),
            &argv(&["cargo", "build"]),
            None,
            None,
            "darwin",
            "aarch64",
            "https://toolchains.example/nightly@2026",
        )
        .expect("redacted toolchain");
        assert_eq!(
            redacted[0].cohort,
            "darwin-aarch64-httpsredactedtoolchains.example/nightlyredacted2026"
        );
    }

    #[test]
    fn target_dir_wins() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let explicit = workspace.path().join("explicit");
        let resources = resolve_output_resources(
            workspace.path(),
            &argv(&[
                "cargo",
                "build",
                "--target-dir",
                explicit.to_str().expect("utf8 path"),
                "--release",
            ]),
            Some("from-env"),
            Some("cargo-build"),
            "linux",
            "x86_64",
            "1.91.0",
        )
        .expect("explicit target dir");
        assert_eq!(
            resources,
            vec![
                resource("cargo_target", explicit, "linux-x86_64-1.91.0"),
                resource(
                    "cargo_build",
                    workspace.path().join("cargo-build"),
                    "linux-x86_64-1.91.0",
                ),
            ]
        );

        let equals = resolve_output_resources(
            workspace.path(),
            &argv(&["rustc", "--target-dir=custom"]),
            Some("from-env"),
            Some("custom"),
            "linux",
            "x86_64",
            "stable",
        )
        .expect("equals form");
        assert_eq!(
            equals,
            vec![resource(
                "cargo_target",
                workspace.path().join("custom"),
                "linux-x86_64-stable",
            )]
        );
    }

    #[test]
    fn nix_command_returns_no_resources() {
        let workspace = tempfile::tempdir().expect("temp dir");
        for command in [
            argv(&["nix", "build", "--target-dir", "not-cargo"]),
            argv(&["/run/current-system/sw/bin/nix-build"]),
        ] {
            let resources = resolve_output_resources(
                workspace.path(),
                &command,
                Some("from-env"),
                Some("cargo-build"),
                "linux",
                "x86_64",
                "stable",
            )
            .expect("nix has no cargo lock");
            assert!(resources.is_empty(), "{command:?} -> {resources:?}");
        }
    }

    #[test]
    fn echo_returns_unknown_output_location() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let err = resolve_output_resources(
            workspace.path(),
            &argv(&["echo", "hello"]),
            None,
            None,
            "darwin",
            "aarch64",
            "stable",
        )
        .expect_err("echo has no output location");
        assert!(err.contains("output location is unknown"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_path_conflicts_with_its_real_path() {
        let root = tempfile::tempdir().expect("temp dir");
        let real = root.path().join("real");
        fs::create_dir(&real).expect("real dir");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let via_link = resource("cargo_target", link, "darwin-aarch64-stable");
        let via_real = resource("cargo_build", real.clone(), "linux-x86_64-other");
        assert!(resources_conflict(&via_link, &via_real));

        let other_cohort = resource("cargo_target", real, "redacted-cohort");
        assert!(resources_conflict(&via_real, &other_cohort));
    }

    #[test]
    fn different_directories_do_not_conflict() {
        let root = tempfile::tempdir().expect("temp dir");
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::create_dir(&left).expect("left dir");
        fs::create_dir(&right).expect("right dir");
        assert!(!resources_conflict(
            &resource("cargo_target", left, "darwin-aarch64-stable"),
            &resource("cargo_target", right, "darwin-aarch64-stable"),
        ));

        let missing_left = root.path().join("missing-left");
        let missing_right = root.path().join("missing-right");
        assert!(!resources_conflict(
            &resource(
                "cargo_target",
                missing_left.clone(),
                "darwin-aarch64-stable"
            ),
            &resource("cargo_build", missing_right, "linux-x86_64-stable"),
        ));
        assert!(resources_conflict(
            &resource(
                "cargo_target",
                missing_left.clone(),
                "darwin-aarch64-stable"
            ),
            &resource("cargo_build", missing_left, "other-cohort"),
        ));
    }
}
