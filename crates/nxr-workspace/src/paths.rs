//! Path normalization and invocation/root relationships.

use camino::{Utf8Path, Utf8PathBuf};

/// Marker file that identifies a Nix flake root.
pub const FLAKE_NIX: &str = "flake.nix";

/// Join `dir` with the `flake.nix` marker filename.
#[must_use]
pub fn flake_nix_path(dir: &Utf8Path) -> Utf8PathBuf {
    dir.join(FLAKE_NIX)
}

/// Return whether `dir` contains a `flake.nix` file.
#[must_use]
pub fn has_flake_nix(dir: &Utf8Path) -> bool {
    flake_nix_path(dir).is_file()
}
