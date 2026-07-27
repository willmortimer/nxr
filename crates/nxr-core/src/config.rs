//! User configuration: defaults, project trust, and secret provider bindings.
//!
//! Secret **values** never appear in these files — only logical refs and paths.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Logical ref → provider binding (user / Home Manager side).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct SecretBinding {
    pub provider: BindingProvider,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

/// Provider ids accepted in `secret-bindings.toml`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingProvider {
    Env,
    File,
    Sops,
    SopsNix,
    Keychain,
    #[serde(rename = "1password")]
    OnePassword,
    Vault,
}

/// Trust policy for a canonical project identity (for example `github.com/org/repo`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct TrustedProject {
    #[serde(default, rename = "allowed_secrets")]
    pub allowed_secrets: Vec<String>,
}

/// Non-secret nxr defaults from `$XDG_CONFIG_HOME/nxr/config.toml`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub trusted_projects: BTreeMap<String, TrustedProject>,
}

/// Logical secret bindings loaded from `secret-bindings.toml` or `[secret_bindings]` in config.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretBindings {
    pub bindings: BTreeMap<String, SecretBinding>,
}

/// Errors while loading user configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("failed to parse config: {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

/// Environment variable overriding the nxr config directory (tests).
pub const NXR_CONFIG_DIR_ENV: &str = "NXR_CONFIG_DIR";

/// Resolve the nxr config directory (`$XDG_CONFIG_HOME/nxr` or platform default).
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var(NXR_CONFIG_DIR_ENV) {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    directories::ProjectDirs::from("", "", "nxr")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/nxr"))
}

/// Load `config.toml` when present; otherwise return defaults.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file exists but cannot be read or parsed.
pub fn load_user_config() -> Result<UserConfig, ConfigError> {
    let path = config_dir().join("config.toml");
    load_toml_file(&path)
}

/// Load secret bindings from `secret-bindings.toml`, falling back to `[secret_bindings]` in config.
///
/// # Errors
///
/// Returns [`ConfigError`] when a bindings file exists but cannot be read or parsed.
pub fn load_secret_bindings() -> Result<SecretBindings, ConfigError> {
    let dir = config_dir();
    let dedicated = dir.join("secret-bindings.toml");
    if dedicated.is_file() {
        let raw: SecretBindingsFile = load_toml_file(&dedicated)?;
        return Ok(raw.into_bindings());
    }

    let config_path = dir.join("config.toml");
    if !config_path.is_file() {
        return Ok(SecretBindings::default());
    }

    let raw: ConfigWithBindings = load_toml_file(&config_path)?;
    Ok(SecretBindings {
        bindings: raw.secret_bindings,
    })
}

#[derive(Deserialize, Default)]
struct SecretBindingsFile {
    #[serde(default)]
    bindings: BTreeMap<String, SecretBinding>,
    #[serde(flatten)]
    flat: BTreeMap<String, toml::Value>,
}

impl SecretBindingsFile {
    fn into_bindings(self) -> SecretBindings {
        if !self.bindings.is_empty() {
            return SecretBindings {
                bindings: self.bindings,
            };
        }
        let bindings = self
            .flat
            .into_iter()
            .filter_map(|(key, value)| {
                if key == "bindings" {
                    return None;
                }
                let binding: SecretBinding = toml::from_str(&value.to_string()).ok()?;
                Some((key, binding))
            })
            .collect();
        SecretBindings { bindings }
    }
}

#[derive(Deserialize, Default)]
struct ConfigWithBindings {
    #[serde(default, rename = "secret_bindings")]
    secret_bindings: BTreeMap<String, SecretBinding>,
}

fn load_toml_file<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T, ConfigError> {
    if !path.is_file() {
        return Ok(T::default());
    }
    let contents = fs::read_to_string(path).map_err(|error| ConfigError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    toml::from_str(&contents).map_err(|error| ConfigError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn secret_bindings_round_trip() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("secret-bindings.toml");
        fs::write(
            &path,
            r#"
[bindings."prod/token"]
provider = "file"
path = "/run/secrets/token"
"#,
        )
        .expect("write");

        let raw: SecretBindingsFile =
            toml::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        let bindings = raw.into_bindings();
        assert_eq!(bindings.bindings.len(), 1);
        assert_eq!(
            bindings.bindings["prod/token"].provider,
            BindingProvider::File
        );
    }
}
