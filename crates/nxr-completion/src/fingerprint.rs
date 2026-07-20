//! Recursive `.nix` tree fingerprint for discovery cache invalidation.

use std::fs;
use std::io;
use std::path::{Component, Path};

use blake3::Hasher;
use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSet, GlobSetBuilder};
use nxr_core::{normalize_repo_relative_path, validate_repo_relative_path};

/// Environment variable with colon-separated glob patterns excluded from fingerprinting.
///
/// Use this to skip huge vendored `.nix` trees that rarely affect discovery.
pub const FINGERPRINT_IGNORE_ENV: &str = "NXR_CACHE_FINGERPRINT_IGNORE";

/// Content digest of all `.nix` files under `flake_root` (hex-encoded BLAKE3).
///
/// Built-in directory ignores match common Nix/Rust artifacts (`.git`, `result`,
/// `target`, …). Additional subtrees may be excluded via [`FINGERPRINT_IGNORE_ENV`].
/// `flake.lock` content is included when present. Non-`.nix` sources are not hashed
/// here; declare extras via `perSystem.nxr.discoveryInputs`.
///
/// # Errors
///
/// Returns an I/O error when the tree cannot be walked or a file cannot be read.
pub fn nix_tree_fingerprint(flake_root: &Utf8Path) -> io::Result<String> {
    let ignore = configured_ignore_globs()?;
    nix_tree_fingerprint_with_ignore(flake_root, &ignore)
}

/// Fingerprint helper for tests and callers supplying explicit ignore globs.
pub(crate) fn nix_tree_fingerprint_with_ignore(
    flake_root: &Utf8Path,
    extra_ignore: &GlobSet,
) -> io::Result<String> {
    let root = canonical_flake_root(flake_root);
    let mut entries = Vec::new();
    walk_nix_files(&root, &root, extra_ignore, &mut entries)?;
    entries.sort();

    let mut hasher = Hasher::new();
    for relative in entries {
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        let bytes = fs::read(root.join(&relative))?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    hash_optional_lock_file(&root, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Content-hash sorted flake-root-relative discovery input paths (hex BLAKE3).
///
/// Missing paths hash as an explicit absence marker so deletion invalidates the
/// cache. Paths are sorted and deduplicated before hashing. Absolute paths,
/// parent traversal, and symlink escapes outside the flake root are rejected.
///
/// # Errors
///
/// Returns an I/O error when a path is invalid, escapes the flake root, or
/// exists but cannot be read.
pub fn discovery_inputs_fingerprint(
    flake_root: &Utf8Path,
    inputs: &[String],
) -> io::Result<String> {
    let root = canonical_flake_root(flake_root);
    let mut paths: Vec<&str> = inputs
        .iter()
        .map(String::as_str)
        .filter(|path| !path.is_empty())
        .collect();
    paths.sort_unstable();
    paths.dedup();

    let mut hasher = Hasher::new();
    for relative in paths {
        validate_repo_relative_path("discoveryInputs", relative)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        let normalized = normalize_repo_relative_path(relative);
        hasher.update(normalized.as_bytes());
        hasher.update(&[0]);
        match read_contained_input(&root, normalized) {
            Ok(bytes) => {
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                hasher.update(b"missing");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn read_contained_input(root: &Utf8Path, relative: &str) -> io::Result<Vec<u8>> {
    let joined = root.join(relative);
    let canonical = match joined.canonicalize_utf8() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Missing files are hashed as absence; reject escapes that still resolve.
            if relative_escapes(relative) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("discovery input `{relative}` escapes flake root"),
                ));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    if !canonical.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("discovery input `{relative}` escapes flake root"),
        ));
    }
    fs::read(canonical)
}

fn relative_escapes(relative: &str) -> bool {
    Path::new(relative)
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
}

fn hash_optional_lock_file(root: &Utf8Path, hasher: &mut Hasher) -> io::Result<()> {
    let lock = root.join("flake.lock");
    match fs::read(&lock) {
        Ok(bytes) => {
            hasher.update(b"flake.lock");
            hasher.update(&[0]);
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn configured_ignore_globs() -> io::Result<GlobSet> {
    let Some(raw) = std::env::var_os(FINGERPRINT_IGNORE_ENV) else {
        return Ok(GlobSet::empty());
    };

    let mut builder = GlobSetBuilder::new();
    for pattern in raw
        .to_string_lossy()
        .split(':')
        .filter(|part| !part.is_empty())
    {
        let glob = Glob::new(pattern).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {FINGERPRINT_IGNORE_ENV} glob `{pattern}`: {error}"),
            )
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn canonical_flake_root(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf())
}

fn walk_nix_files(
    root: &Utf8Path,
    dir: &Utf8Path,
    extra_ignore: &GlobSet,
    entries: &mut Vec<String>,
) -> io::Result<()> {
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            let Some(utf8_path) = Utf8Path::from_path(&path) else {
                continue;
            };

            if should_ignore(root, utf8_path, extra_ignore) {
                continue;
            }

            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(utf8_path.to_path_buf());
                continue;
            }

            if file_type.is_file() && is_nix_file(utf8_path) {
                let relative = utf8_path
                    .strip_prefix(root)
                    .unwrap_or(utf8_path)
                    .as_str()
                    .to_owned();
                entries.push(relative);
            }
        }
    }

    Ok(())
}

fn is_nix_file(path: &Utf8Path) -> bool {
    path.extension().is_some_and(|ext| ext == "nix")
}

fn should_ignore(root: &Utf8Path, path: &Utf8Path, extra_ignore: &GlobSet) -> bool {
    if is_builtin_ignored(path) {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = relative_for_glob(relative.as_str());
    extra_ignore.is_match(relative)
}

fn is_builtin_ignored(path: &Utf8Path) -> bool {
    if path.starts_with("/nix/store") {
        return true;
    }

    path.components().any(|component| {
        let name = component.as_str();
        name == ".git"
            || name == ".direnv"
            || name == ".cache"
            || name == "node_modules"
            || name == "target"
            || name == "result"
            || name.starts_with("result-")
    })
}

fn relative_for_glob(relative: &str) -> &str {
    relative.strip_prefix("./").unwrap_or(relative)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{
        discovery_inputs_fingerprint, nix_tree_fingerprint, nix_tree_fingerprint_with_ignore,
    };
    use globset::{Glob, GlobSetBuilder};

    fn utf8_root(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path")
    }

    #[test]
    fn imported_nix_change_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("write flake");
        fs::create_dir_all(root.join("nix")).expect("mkdir");
        fs::write(root.join("nix/apps.nix"), "1\n").expect("write apps");
        let initial = nix_tree_fingerprint(&root).expect("fingerprint");
        fs::write(root.join("nix/apps.nix"), "2\n").expect("edit apps");
        let updated = nix_tree_fingerprint(&root).expect("fingerprint after edit");
        assert_ne!(initial, updated);
    }

    #[test]
    fn content_change_same_length_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{ outputs = {}; }\n").expect("write flake");
        fs::write(root.join("a.nix"), "aaaa\n").expect("write");
        let initial = nix_tree_fingerprint(&root).expect("fingerprint");
        // Same byte length, different contents (mtime-only fingerprints would miss this
        // when clocks are coarse).
        fs::write(root.join("a.nix"), "bbbb\n").expect("rewrite");
        let updated = nix_tree_fingerprint(&root).expect("fingerprint after edit");
        assert_ne!(initial, updated);
    }

    #[test]
    fn fingerprint_ignore_env_skips_matching_subtree() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
        fs::create_dir_all(root.join("vendor")).expect("mkdir");
        fs::write(root.join("vendor/x.nix"), "1\n").expect("write vendor");
        let with_vendor = nix_tree_fingerprint(&root).expect("fingerprint with vendor");

        let mut builder = GlobSetBuilder::new();
        builder.add(Glob::new("vendor/**").expect("glob"));
        let ignore = builder.build().expect("build");
        let without_vendor =
            nix_tree_fingerprint_with_ignore(&root, &ignore).expect("fingerprint ignoring vendor");
        assert_ne!(with_vendor, without_vendor);
    }

    #[test]
    fn symlink_flake_root_matches_canonical_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
        let link = temp.path().join("link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), &link).expect("symlink");
        }
        #[cfg(not(unix))]
        {
            return;
        }
        let canonical = nix_tree_fingerprint(&root).expect("canonical fingerprint");
        let linked =
            nix_tree_fingerprint(&camino::Utf8PathBuf::from_path_buf(link).expect("utf8 link"))
                .expect("symlink fingerprint");
        assert_eq!(canonical, linked);
    }

    #[test]
    fn flake_lock_change_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
        let baseline = nix_tree_fingerprint(&root).expect("baseline");
        fs::write(root.join("flake.lock"), "{}\n").expect("write lock");
        let changed = nix_tree_fingerprint(&root).expect("changed");
        assert_ne!(baseline, changed);
    }

    #[test]
    fn flake_lock_atomic_replace_changes_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("flake.nix"), "{}\n").expect("write flake");
        fs::write(root.join("flake.lock"), "v1\n").expect("write lock");
        let initial = nix_tree_fingerprint(&root).expect("initial fingerprint");
        // Brief pause so coarse filesystems still observe a distinct write.
        thread::sleep(Duration::from_millis(5));
        let tmp = root.join("flake.lock.tmp");
        fs::write(&tmp, "v2\n").expect("write tmp");
        fs::rename(&tmp, root.join("flake.lock")).expect("rename");
        let updated = nix_tree_fingerprint(&root).expect("updated fingerprint");
        assert_ne!(initial, updated);
    }

    #[test]
    fn discovery_input_content_change_invalidates() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        fs::write(root.join("extra.txt"), "one\n").expect("write");
        let inputs = vec!["extra.txt".to_owned()];
        let initial = discovery_inputs_fingerprint(&root, &inputs).expect("initial");
        fs::write(root.join("extra.txt"), "two\n").expect("rewrite");
        let updated = discovery_inputs_fingerprint(&root, &inputs).expect("updated");
        assert_ne!(initial, updated);
    }

    #[test]
    fn discovery_input_rejects_parent_escape() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        let err =
            discovery_inputs_fingerprint(&root, &["../escape".to_owned()]).expect_err("escape");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
