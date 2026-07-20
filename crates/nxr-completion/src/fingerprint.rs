//! Recursive `.nix` tree fingerprint for discovery cache invalidation.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Environment variable with colon-separated glob patterns excluded from fingerprinting.
///
/// Use this to skip huge vendored `.nix` trees that rarely affect discovery.
pub const FINGERPRINT_IGNORE_ENV: &str = "NXR_CACHE_FINGERPRINT_IGNORE";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct NixFileStamp {
    len: u64,
    secs: u64,
    nanos: u32,
}

impl NixFileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        let len = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let duration = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            len,
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        }
    }
}

/// Fingerprint all `.nix` files under `flake_root` using relative path, length, and mtime.
///
/// Built-in directory ignores match common Nix/Rust artifacts (`.git`, `result`,
/// `target`, …). Additional subtrees may be excluded via [`FINGERPRINT_IGNORE_ENV`].
///
/// # Errors
///
/// Returns an I/O error when the tree cannot be walked.
pub fn nix_tree_fingerprint(flake_root: &Utf8Path) -> io::Result<u64> {
    let ignore = configured_ignore_globs()?;
    nix_tree_fingerprint_with_ignore(flake_root, &ignore)
}

/// Fingerprint helper for tests and callers supplying explicit ignore globs.
pub(crate) fn nix_tree_fingerprint_with_ignore(
    flake_root: &Utf8Path,
    extra_ignore: &GlobSet,
) -> io::Result<u64> {
    let root = canonical_flake_root(flake_root);
    let mut entries = Vec::new();
    walk_nix_files(&root, &root, extra_ignore, &mut entries)?;
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut hasher = DefaultHasher::new();
    for (path, stamp) in entries {
        path.hash(&mut hasher);
        stamp.hash(&mut hasher);
    }
    hash_optional_lock_file(&root, &mut hasher)?;
    Ok(hasher.finish())
}

fn hash_optional_lock_file(root: &Utf8Path, hasher: &mut DefaultHasher) -> io::Result<()> {
    let lock = root.join("flake.lock");
    match fs::metadata(&lock) {
        Ok(metadata) => {
            "flake.lock".hash(hasher);
            NixFileStamp::from_metadata(&metadata).hash(hasher);
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
    entries: &mut Vec<(String, NixFileStamp)>,
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
                let stamp = NixFileStamp::from_metadata(&entry.metadata()?);
                entries.push((relative, stamp));
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

    use super::{nix_tree_fingerprint, nix_tree_fingerprint_with_ignore};
    use globset::{Glob, GlobSetBuilder};

    fn utf8_root(temp: &TempDir) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path")
    }

    fn write_flake_tree(root: &camino::Utf8Path) {
        fs::write(root.join("flake.nix"), "import ./nix/apps.nix").expect("flake.nix");
        fs::create_dir_all(root.join("nix")).expect("nix dir");
        fs::write(root.join("nix/apps.nix"), "{ }").expect("apps.nix");
    }

    #[test]
    fn imported_nix_change_invalidates_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        write_flake_tree(&root);

        let initial = nix_tree_fingerprint(&root).expect("fingerprint");
        fs::write(root.join("nix/apps.nix"), "{ changed = true; }").expect("edit apps.nix");
        let updated = nix_tree_fingerprint(&root).expect("fingerprint after edit");

        assert_ne!(initial, updated);
    }

    #[test]
    fn fingerprint_ignore_env_skips_matching_subtree() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        write_flake_tree(&root);
        fs::create_dir_all(root.join("vendor/nix")).expect("vendor dir");
        fs::write(root.join("vendor/nix/huge.nix"), "{ }").expect("huge.nix");

        let with_vendor = nix_tree_fingerprint(&root).expect("fingerprint with vendor");

        let ignore = GlobSetBuilder::new()
            .add(Glob::new("vendor/**").expect("glob"))
            .build()
            .expect("ignore set");
        let ignored =
            nix_tree_fingerprint_with_ignore(&root, &ignore).expect("fingerprint ignoring vendor");

        assert_ne!(with_vendor, ignored);
    }

    #[test]
    fn symlink_flake_root_matches_canonical_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        write_flake_tree(&root);

        let links = temp.path().join("links");
        fs::create_dir_all(&links).expect("links dir");
        let link = links.join("flake-link");
        std::os::unix::fs::symlink(&root, &link).expect("symlink");

        let canonical = nix_tree_fingerprint(&root).expect("canonical fingerprint");
        let via_link =
            nix_tree_fingerprint(&camino::Utf8PathBuf::from_path_buf(link).expect("utf8 link"))
                .expect("symlink fingerprint");

        assert_eq!(canonical, via_link);
    }

    #[test]
    fn builtin_ignores_skip_git_and_target_trees() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        write_flake_tree(&root);
        fs::create_dir_all(root.join(".git/objects")).expect("git dir");
        fs::write(root.join(".git/objects/secret.nix"), "{ }").expect("ignored nix");
        fs::create_dir_all(root.join("target/debug")).expect("target dir");
        fs::write(root.join("target/debug/pkg.nix"), "{ }").expect("ignored nix");

        let baseline = nix_tree_fingerprint(&root).expect("baseline");
        fs::write(root.join("tracked.nix"), "{ }").expect("tracked nix");
        let changed = nix_tree_fingerprint(&root).expect("changed");

        assert_ne!(baseline, changed);
    }

    #[test]
    fn flake_lock_atomic_replace_changes_fingerprint() {
        let temp = TempDir::new().expect("tempdir");
        let root = utf8_root(&temp);
        write_flake_tree(&root);

        let lock_path = root.join("flake.lock");
        fs::write(&lock_path, "{}").expect("initial lock");

        let initial = nix_tree_fingerprint(&root).expect("initial fingerprint");

        let temp_lock = root.join(".flake.lock.tmp");
        fs::write(&temp_lock, "{ \"nodes\": {} }").expect("new lock body");
        fs::rename(&temp_lock, &lock_path).expect("atomic replace");

        thread::sleep(Duration::from_millis(10));
        let updated = nix_tree_fingerprint(&root).expect("updated fingerprint");

        assert_ne!(initial, updated);
    }
}
