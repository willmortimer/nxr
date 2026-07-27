//! Project trust database for secret-bearing and confirmation-gated execution.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Environment variable that grants one-shot trust for the current invocation.
pub const NXR_TRUST_PROJECT_ENV: &str = "NXR_TRUST_PROJECT";

const TRUST_DB_SCHEMA_VERSION: u32 = 1;
const TRUST_DB_FILENAME: &str = "trusted-projects.json";

/// Errors while loading, saving, or enforcing project trust.
#[derive(Debug, Error)]
pub enum TrustError {
    #[error("failed to canonicalize project path `{path}`: {source}")]
    Canonicalize {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("project trust database is unavailable on this host")]
    DatabaseUnavailable,
    #[error("failed to read project trust database: {0}")]
    Read(#[source] io::Error),
    #[error("failed to write project trust database: {0}")]
    Write(#[source] io::Error),
    #[error("failed to parse project trust database: {0}")]
    Parse(#[source] serde_json::Error),
    #[error(
        "project `{display}` is not trusted (run `nxr trust add` or set {NXR_TRUST_PROJECT_ENV}=1)"
    )]
    NotTrusted { display: String },
    #[error(
        "project `{display}` requires trust approval but stdin is not interactive (run `nxr trust add` or set {NXR_TRUST_PROJECT_ENV}=1)"
    )]
    ApprovalRequiredNonInteractive { display: String },
    #[error("project trust approval declined for `{display}`")]
    ApprovalDeclined { display: String },
    #[error("failed to read trust approval input: {0}")]
    ApprovalIo(#[source] io::Error),
}

/// On-disk project trust database keyed by canonical flake root (or remote `nix_ref`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustDatabase {
    schema_version: u32,
    trusted_projects: BTreeMap<String, TrustedProjectRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct TrustedProjectRecord {
    trusted_at: String,
}

impl TrustDatabase {
    /// Load the trust database from the user config directory.
    ///
    /// Returns an empty database when the file does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the config directory is unavailable or the file is invalid.
    pub fn load() -> Result<Self, TrustError> {
        let path = trust_db_path().ok_or(TrustError::DatabaseUnavailable)?;
        Self::load_from(&path)
    }

    /// Load the trust database from `path`.
    ///
    /// Returns an empty database when the file does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the file cannot be read or parsed.
    pub fn load_from(path: &Path) -> Result<Self, TrustError> {
        if !path.is_file() {
            return Ok(Self::empty());
        }
        let contents = fs::read_to_string(path).map_err(TrustError::Read)?;
        let database: Self = serde_json::from_str(&contents).map_err(TrustError::Parse)?;
        if database.schema_version != TRUST_DB_SCHEMA_VERSION {
            return Ok(Self::empty());
        }
        Ok(database)
    }

    /// Persist the trust database to the user config directory.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the config directory is unavailable or the file cannot be written.
    pub fn save(&self) -> Result<(), TrustError> {
        let path = trust_db_path().ok_or(TrustError::DatabaseUnavailable)?;
        self.save_to(&path)
    }

    /// Persist the trust database to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when parent directories cannot be created or the file cannot be written.
    pub fn save_to(&self, path: &Path) -> Result<(), TrustError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(TrustError::Write)?;
        }
        let rendered = serde_json::to_string_pretty(self).map_err(TrustError::Parse)?;
        fs::write(path, rendered).map_err(TrustError::Write)
    }

    /// Returns whether `project_key` is trusted.
    #[must_use]
    pub fn is_trusted(&self, project_key: &str) -> bool {
        self.trusted_projects.contains_key(project_key)
    }

    /// Mark `project_key` as trusted and persist the database.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the database cannot be written.
    pub fn add_trust(&mut self, project_key: &str) -> Result<(), TrustError> {
        self.trusted_projects.insert(
            project_key.to_owned(),
            TrustedProjectRecord {
                trusted_at: chrono_like_timestamp(),
            },
        );
        self.save()
    }

    /// Mark `project_key` as trusted without persisting.
    pub fn add_trust_in_memory(&mut self, project_key: &str) {
        self.trusted_projects.insert(
            project_key.to_owned(),
            TrustedProjectRecord {
                trusted_at: chrono_like_timestamp(),
            },
        );
    }

    /// Persist in-memory trust changes to the user config directory.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the database cannot be written.
    pub fn persist(&self) -> Result<(), TrustError> {
        self.save()
    }

    /// Remove trust for `project_key` and persist the database.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the database cannot be written.
    pub fn revoke_trust(&mut self, project_key: &str) -> Result<(), TrustError> {
        self.trusted_projects.remove(project_key);
        self.save()
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema_version: TRUST_DB_SCHEMA_VERSION,
            trusted_projects: BTreeMap::new(),
        }
    }
}

/// Canonical filesystem key for a local flake root.
///
/// # Errors
///
/// Returns [`TrustError`] when the path cannot be canonicalized.
pub fn canonical_project_key(path: &Path) -> Result<String, TrustError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| TrustError::Canonicalize {
            path: path.display().to_string(),
            source,
        })?;
    Ok(canonical.to_string_lossy().into_owned())
}

/// Resolve the trust key for a local flake root or remote flake reference.
#[must_use]
pub fn project_trust_key(local_root: Option<&Path>, nix_ref: &str) -> String {
    match local_root {
        Some(path) => canonical_project_key(path).unwrap_or_else(|_| path.display().to_string()),
        None => nix_ref.to_owned(),
    }
}

/// Returns true when `NXR_TRUST_PROJECT` grants one-shot trust for this invocation.
#[must_use]
pub fn trust_project_env_enabled() -> bool {
    match std::env::var(NXR_TRUST_PROJECT_ENV) {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !normalized.is_empty() && !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// Require project trust before executing secret-bearing or confirmation-gated tasks.
///
/// Persists approval when the user accepts on a TTY. `NXR_TRUST_PROJECT=1` grants
/// one-shot trust without writing the database.
///
/// # Errors
///
/// Returns [`TrustError`] when trust is missing, stdin is not interactive, or the user declines.
pub fn enforce_project_trust(
    project_key: &str,
    display: &str,
    database: &TrustDatabase,
) -> Result<(), TrustError> {
    if database.is_trusted(project_key) || trust_project_env_enabled() {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(TrustError::ApprovalRequiredNonInteractive {
            display: display.to_owned(),
        });
    }

    let prompt = format!(
        "Project {display} requests secrets and/or confirmation-gated tasks.\nTrust this project? [y/N] "
    );
    let mut stderr = io::stderr().lock();
    stderr
        .write_all(prompt.as_bytes())
        .map_err(TrustError::ApprovalIo)?;
    stderr.flush().map_err(TrustError::ApprovalIo)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(TrustError::ApprovalIo)?;
    let normalized = answer.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "y" | "yes") {
        let mut updated = database.clone();
        updated.add_trust(project_key)?;
        Ok(())
    } else {
        Err(TrustError::ApprovalDeclined {
            display: display.to_owned(),
        })
    }
}

fn trust_db_path() -> Option<PathBuf> {
    resolve_trust_db_path(
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .as_deref(),
    )
}

fn resolve_trust_db_path(config_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(config_home) = config_home {
        return Some(config_home.join("nxr").join(TRUST_DB_FILENAME));
    }
    directories::ProjectDirs::from("dev", "nxr", "nxr")
        .map(|dirs| dirs.config_dir().join(TRUST_DB_FILENAME))
}

fn chrono_like_timestamp() -> String {
    // Avoid pulling in chrono: coarse wall-clock string is enough for audit metadata.
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::{TrustDatabase, canonical_project_key, enforce_project_trust, project_trust_key};

    #[test]
    fn project_trust_key_prefers_canonical_local_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("flake");
        std::fs::create_dir_all(&root).expect("create root");
        let canonical = canonical_project_key(&root).expect("canonicalize");
        assert_eq!(project_trust_key(Some(&root), "path:ignored"), canonical);
    }

    #[test]
    fn project_trust_key_falls_back_to_nix_ref_for_remote_flakes() {
        assert_eq!(
            project_trust_key(None, "github:owner/repo"),
            "github:owner/repo"
        );
    }

    #[test]
    fn enforce_project_trust_skips_when_already_trusted() {
        let mut database = TrustDatabase::empty();
        database.trusted_projects.insert(
            "/tmp/project".to_owned(),
            super::TrustedProjectRecord {
                trusted_at: "0".to_owned(),
            },
        );
        enforce_project_trust("/tmp/project", ".", &database).expect("already trusted");
    }

    #[test]
    fn empty_database_round_trips_through_save_and_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("trusted-projects.json");

        let mut database = TrustDatabase::empty();
        database.add_trust_in_memory("/tmp/project");
        database.save_to(&path).expect("persist trust");

        let loaded = TrustDatabase::load_from(&path).expect("load trust db");
        assert!(loaded.is_trusted("/tmp/project"));
    }

    #[test]
    fn resolve_trust_db_path_honors_xdg_config_home() {
        let temp = tempfile::tempdir().expect("tempdir");
        let expected = temp.path().join("nxr").join("trusted-projects.json");
        assert_eq!(
            super::resolve_trust_db_path(Some(temp.path())),
            Some(expected)
        );
    }
}
