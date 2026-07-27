//! Runtime secret resolution and secure delivery (schema v2).
//!
//! Values are resolved only at child spawn. Plans and events never include them.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use nxr_core::config::{BindingProvider, SecretBinding, SecretBindings, TrustedProject};
use tempfile::NamedTempFile;

use crate::context::{ContextError, PlanSecretEntry};
use crate::schema::{SecretDelivery, SecretProvider};

/// Resolved secret material ready for child delivery (never log contents).
#[derive(Debug)]
pub struct ResolvedSecrets {
    pub env_overrides: BTreeMap<String, String>,
    /// Tempfiles deleted when this guard is dropped (after child exit).
    pub temp_files: Vec<SecureTempFile>,
    /// Payload written to child stdin once when `delivery = "stdin"`.
    pub stdin_payload: Option<Vec<u8>>,
}

/// A mode-0600 tempfile removed on drop.
#[derive(Debug)]
pub struct SecureTempFile {
    path: PathBuf,
}

impl Drop for SecureTempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl SecureTempFile {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `contents` to a new secure tempfile (Unix mode 0600).
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::ResolveIo`] when the tempfile cannot be created or written.
    pub fn write(contents: &[u8]) -> Result<Self, ContextError> {
        let mut file = NamedTempFile::new().map_err(|error| ContextError::ResolveIo {
            message: error.to_string(),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(file.path(), perms).map_err(|error| ContextError::ResolveIo {
                message: error.to_string(),
            })?;
        }
        file.write_all(contents)
            .map_err(|error| ContextError::ResolveIo {
                message: error.to_string(),
            })?;
        file.flush().map_err(|error| ContextError::ResolveIo {
            message: error.to_string(),
        })?;
        let (_file, path) = file.keep().map_err(|error| ContextError::ResolveIo {
            message: error.to_string(),
        })?;
        Ok(Self { path })
    }
}

/// Verify every logical `ref` is authorized for `project_id` before resolution.
///
/// # Errors
///
/// Returns [`ContextError::UnauthorizedSecrets`] when trust is missing or incomplete.
pub fn authorize_secret_refs(
    project_id: &str,
    refs: &[String],
    trusted_projects: &BTreeMap<String, TrustedProject>,
) -> Result<(), ContextError> {
    if refs.is_empty() {
        return Ok(());
    }
    let Some(trust) = trusted_projects.get(project_id) else {
        return Err(ContextError::UnauthorizedSecrets {
            project: project_id.to_owned(),
            missing_refs: refs.to_vec(),
        });
    };
    let mut missing = Vec::new();
    for reference in refs {
        if !trust
            .allowed_secrets
            .iter()
            .any(|allowed| allowed == reference)
        {
            missing.push(reference.clone());
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ContextError::UnauthorizedSecrets {
            project: project_id.to_owned(),
            missing_refs: missing,
        })
    }
}

/// Resolve plan secret entries using bindings and deliver per slot delivery mode.
///
/// # Errors
///
/// Returns [`ContextError`] on missing secrets, unsupported providers, or delivery conflicts.
pub fn resolve_context_secrets(
    entries: &[PlanSecretEntry],
    bindings: &SecretBindings,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedSecrets, ContextError> {
    let mut env_overrides = BTreeMap::new();
    let mut temp_files = Vec::new();
    let mut stdin_payload = None;

    for entry in entries {
        let value = resolve_secret_value(entry, bindings, &env_lookup)?;
        match entry.delivery {
            SecretDelivery::Env => {
                env_overrides.insert(entry.name.clone(), value);
            }
            SecretDelivery::File => {
                let path = deliver_as_file(&entry.name, &value, &mut temp_files)?;
                env_overrides.insert(entry.name.clone(), path);
            }
            SecretDelivery::Stdin => {
                if stdin_payload.is_some() {
                    return Err(ContextError::MultipleStdinSecrets);
                }
                stdin_payload = Some(value.into_bytes());
            }
        }
    }

    Ok(ResolvedSecrets {
        env_overrides,
        temp_files,
        stdin_payload,
    })
}

fn resolve_secret_value(
    entry: &PlanSecretEntry,
    bindings: &SecretBindings,
    env_lookup: &impl Fn(&str) -> Option<String>,
) -> Result<String, ContextError> {
    match entry.provider {
        SecretProvider::Env => env_lookup(&entry.reference).ok_or(ContextError::MissingSecret {
            slot: entry.name.clone(),
            reference: entry.reference.clone(),
        }),
        SecretProvider::File | SecretProvider::Sops | SecretProvider::SopsNix => {
            let binding =
                bindings
                    .bindings
                    .get(&entry.reference)
                    .ok_or(ContextError::MissingBinding {
                        slot: entry.name.clone(),
                        reference: entry.reference.clone(),
                    })?;
            resolve_binding_value(binding, &entry.reference, &entry.name)
        }
    }
}

fn resolve_binding_value(
    binding: &SecretBinding,
    reference: &str,
    slot: &str,
) -> Result<String, ContextError> {
    match binding.provider {
        BindingProvider::Env => {
            let env_name = binding
                .env
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(reference);
            std::env::var(env_name).map_err(|_| ContextError::MissingSecret {
                slot: slot.to_owned(),
                reference: reference.to_owned(),
            })
        }
        BindingProvider::File | BindingProvider::SopsNix => {
            let path = binding
                .path
                .as_deref()
                .ok_or(ContextError::MissingBinding {
                    slot: slot.to_owned(),
                    reference: reference.to_owned(),
                })?;
            if binding.provider == BindingProvider::SopsNix {
                Ok(path.to_owned())
            } else {
                fs::read_to_string(path).map_err(|error| ContextError::ResolveIo {
                    message: error.to_string(),
                })
            }
        }
        BindingProvider::Sops => {
            let path = binding
                .path
                .as_deref()
                .ok_or(ContextError::MissingBinding {
                    slot: slot.to_owned(),
                    reference: reference.to_owned(),
                })?;
            let key = binding.key.as_deref().unwrap_or("value");
            decrypt_sops_key(path, key, slot, reference)
        }
        BindingProvider::Keychain | BindingProvider::OnePassword | BindingProvider::Vault => {
            Err(ContextError::UnsupportedBindingProvider {
                slot: slot.to_owned(),
                reference: reference.to_owned(),
                provider: binding.provider,
            })
        }
    }
}

fn deliver_as_file(
    slot: &str,
    value: &str,
    temp_files: &mut Vec<SecureTempFile>,
) -> Result<String, ContextError> {
    let path = Path::new(value);
    if path.is_file() {
        return Ok(value.to_owned());
    }
    let temp = SecureTempFile::write(value.as_bytes())?;
    let path = temp.path().to_string_lossy().into_owned();
    temp_files.push(temp);
    let _ = slot;
    Ok(path)
}

fn decrypt_sops_key(
    path: &str,
    key: &str,
    slot: &str,
    reference: &str,
) -> Result<String, ContextError> {
    let extract = format!(r#"["{key}"]"#);
    let output = Command::new("sops")
        .args(["-d", "--extract", &extract, path])
        .output()
        .map_err(|error| ContextError::ResolveIo {
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(ContextError::SopsDecrypt {
            slot: slot.to_owned(),
            reference: reference.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let value = String::from_utf8(output.stdout).map_err(|error| ContextError::ResolveIo {
        message: error.to_string(),
    })?;
    Ok(value.trim_end_matches('\n').to_owned())
}

/// Collect logical refs from plan secret entries (for authorization).
#[must_use]
pub fn secret_refs_for_entries(entries: &[PlanSecretEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry.reference.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PlanSecretValuePlaceholder;
    use nxr_core::config::SecretBinding;

    fn entry(
        name: &str,
        reference: &str,
        delivery: SecretDelivery,
        provider: SecretProvider,
    ) -> PlanSecretEntry {
        PlanSecretEntry {
            name: name.to_owned(),
            reference: reference.to_owned(),
            delivery,
            provider,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }
    }

    #[test]
    fn env_provider_unchanged() {
        let resolved = resolve_context_secrets(
            &[entry(
                "DEPLOY_TOKEN",
                "NXR_TEST_TOKEN",
                SecretDelivery::Env,
                SecretProvider::Env,
            )],
            &SecretBindings::default(),
            |name| {
                if name == "NXR_TEST_TOKEN" {
                    Some("secret-value".to_owned())
                } else {
                    None
                }
            },
        )
        .expect("resolve");
        assert_eq!(
            resolved
                .env_overrides
                .get("DEPLOY_TOKEN")
                .map(String::as_str),
            Some("secret-value")
        );
        assert!(resolved.stdin_payload.is_none());
    }

    #[test]
    fn file_delivery_uses_secure_tempfile() {
        let resolved = resolve_context_secrets(
            &[entry(
                "KUBECONFIG",
                "NXR_TEST_TOKEN",
                SecretDelivery::File,
                SecretProvider::Env,
            )],
            &SecretBindings::default(),
            |_| Some("kubeconfig-contents".to_owned()),
        )
        .expect("resolve");
        let path = resolved.env_overrides.get("KUBECONFIG").expect("path env");
        assert!(Path::new(path).is_file());
        let contents = fs::read_to_string(path).expect("read tempfile");
        assert_eq!(contents, "kubeconfig-contents");
    }

    #[test]
    fn stdin_delivery_sets_payload() {
        let resolved = resolve_context_secrets(
            &[entry(
                "TOKEN",
                "NXR_TEST_TOKEN",
                SecretDelivery::Stdin,
                SecretProvider::Env,
            )],
            &SecretBindings::default(),
            |_| Some("stdin-secret".to_owned()),
        )
        .expect("resolve");
        assert_eq!(
            resolved.stdin_payload.as_deref(),
            Some(b"stdin-secret".as_slice())
        );
    }

    #[test]
    fn file_binding_reads_path_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret_path = dir.path().join("secret.txt");
        fs::write(&secret_path, "from-file").expect("write");
        let mut bindings = SecretBindings::default();
        bindings.bindings.insert(
            "prod/token".to_owned(),
            SecretBinding {
                provider: BindingProvider::File,
                path: Some(secret_path.to_string_lossy().into_owned()),
                env: None,
                key: None,
            },
        );
        let resolved = resolve_context_secrets(
            &[entry(
                "TOKEN",
                "prod/token",
                SecretDelivery::Env,
                SecretProvider::File,
            )],
            &bindings,
            |_| None,
        )
        .expect("resolve");
        assert_eq!(
            resolved.env_overrides.get("TOKEN").map(String::as_str),
            Some("from-file")
        );
    }

    #[test]
    fn unsupported_binding_provider_errors() {
        let mut bindings = SecretBindings::default();
        bindings.bindings.insert(
            "prod/token".to_owned(),
            SecretBinding {
                provider: BindingProvider::Keychain,
                path: None,
                env: None,
                key: None,
            },
        );
        let error = resolve_context_secrets(
            &[entry(
                "TOKEN",
                "prod/token",
                SecretDelivery::Env,
                SecretProvider::Sops,
            )],
            &bindings,
            |_| None,
        )
        .expect_err("unsupported");
        assert!(matches!(
            error,
            ContextError::UnsupportedBindingProvider { .. }
        ));
    }

    #[test]
    fn authorize_requires_trusted_refs() {
        let mut trusted = BTreeMap::new();
        trusted.insert(
            "github.com/org/repo".to_owned(),
            TrustedProject {
                allowed_secrets: vec!["prod/token".to_owned()],
            },
        );
        authorize_secret_refs("github.com/org/repo", &["prod/token".to_owned()], &trusted)
            .expect("authorized");
        let error =
            authorize_secret_refs("github.com/org/repo", &["other/ref".to_owned()], &trusted)
                .expect_err("missing");
        assert!(matches!(error, ContextError::UnauthorizedSecrets { .. }));
    }
}
