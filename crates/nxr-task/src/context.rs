//! Execution-context environment and secret resolution (schema v2).
//!
//! Secret **values** are resolved only at child spawn time via the env provider.
//! Plans and events carry slot names, logical refs, and delivery modes only.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, IsTerminal, Write};

use nxr_core::EnvironmentPolicy;
use serde::{Deserialize, Serialize};

use crate::schema::{
    ContextEnvironment, ContextEnvironmentMode, ContextSecretRef, ExecutionContext, SecretDelivery,
    SecretProvider, TaskDocument,
};

/// Environment variable that skips interactive context confirmation prompts.
pub const NXR_ASSUME_YES_ENV: &str = "NXR_ASSUME_YES";

/// Errors while applying or resolving execution contexts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextError {
    /// A task references a context name that is not defined.
    UnknownContext { task: String, context: String },
    /// A required secret reference is not present in the caller environment.
    MissingSecret { slot: String, reference: String },
    /// A declared delivery mode is not implemented in this runtime slice.
    UnsupportedDelivery {
        slot: String,
        reference: String,
        delivery: SecretDelivery,
    },
    /// A declared secret provider is not implemented in this runtime slice.
    UnsupportedProvider {
        slot: String,
        reference: String,
        provider: SecretProvider,
    },
    /// A context requires confirmation but stdin is not interactive.
    ConfirmRequiredNonInteractive { context: String, task: String },
    /// A context confirmation prompt was declined.
    ConfirmDeclined { context: String, task: String },
    /// Failed to read confirmation input from stdin.
    ConfirmIo { message: String },
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownContext { task, context } => {
                write!(f, "task {task} references undefined context {context}")
            }
            Self::MissingSecret { slot, reference } => write!(
                f,
                "required secret not found in caller environment: slot {slot}, ref {reference}"
            ),
            Self::UnsupportedDelivery {
                slot,
                reference,
                delivery,
            } => write!(
                f,
                "secret delivery mode {:?} is not implemented yet (slot {slot}, ref {reference})",
                delivery_label(*delivery)
            ),
            Self::UnsupportedProvider {
                slot,
                reference,
                provider,
            } => write!(
                f,
                "secret provider {:?} is not implemented yet (slot {slot}, ref {reference})",
                provider_label(*provider)
            ),
            Self::ConfirmRequiredNonInteractive { context, task } => write!(
                f,
                "context {context} for task {task} requires confirmation but stdin is not interactive (set {NXR_ASSUME_YES_ENV}=1 or run in a terminal)"
            ),
            Self::ConfirmDeclined { context, task } => {
                write!(f, "context {context} confirmation declined for task {task}")
            }
            Self::ConfirmIo { message } => {
                write!(f, "failed to read context confirmation: {message}")
            }
        }
    }
}

impl std::error::Error for ContextError {}

fn delivery_label(delivery: SecretDelivery) -> &'static str {
    match delivery {
        SecretDelivery::Env => "env",
        SecretDelivery::File => "file",
        SecretDelivery::Stdin => "stdin",
    }
}

fn provider_label(provider: SecretProvider) -> &'static str {
    match provider {
        SecretProvider::Env => "env",
        SecretProvider::File => "file",
        SecretProvider::Sops => "sops",
        SecretProvider::SopsNix => "sops-nix",
    }
}

/// Effective delivery mode for a context secret (schema default: `env`).
#[must_use]
pub fn secret_delivery_mode(secret: &ContextSecretRef) -> SecretDelivery {
    secret.delivery.unwrap_or(SecretDelivery::Env)
}

/// Effective secret provider for a context secret (schema default: `env`).
#[must_use]
pub fn secret_provider_mode(secret: &ContextSecretRef) -> SecretProvider {
    secret.provider
}

/// Secret metadata recorded in plans (never includes resolved values).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanSecretEntry {
    pub name: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub delivery: SecretDelivery,
    pub provider: SecretProvider,
    /// Placeholder indicating resolution happens at runtime.
    pub value: PlanSecretValuePlaceholder,
}

/// Wire placeholder for secret values in serialized plans.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename = "value")]
pub enum PlanSecretValuePlaceholder {
    #[serde(rename = "<runtime>")]
    Runtime,
}

impl PlanSecretValuePlaceholder {
    pub const RUNTIME: Self = Self::Runtime;
}

/// Result of applying a task's execution context during preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedTaskContext {
    pub context_name: String,
    pub environment_policy: EnvironmentPolicy,
    /// Non-secret `environment.set` entries applied at spawn for inherit-mode contexts.
    pub spawn_env_set: BTreeMap<String, String>,
    pub plan_secrets: Vec<PlanSecretEntry>,
    /// Resolved devShell name from the context (when set).
    pub shell: Option<String>,
    /// Whether the runner should prompt before executing this node.
    pub confirm: bool,
}

/// Look up the named context for `task_id` or return [`ContextError::UnknownContext`].
///
/// # Errors
///
/// Returns [`ContextError::UnknownContext`] when the task names a missing context.
pub fn resolve_task_context<'a>(
    document: &'a TaskDocument,
    task_id: &str,
    context_name: &str,
) -> Result<&'a ExecutionContext, ContextError> {
    document
        .contexts
        .get(context_name)
        .ok_or_else(|| ContextError::UnknownContext {
            task: task_id.to_owned(),
            context: context_name.to_owned(),
        })
}

/// Build plan metadata and environment policy for a task node with an execution context.
///
/// Does **not** resolve secret values (planning / dry-run safe).
///
/// # Errors
///
/// Returns [`ContextError`] when the context name is unknown.
pub fn apply_task_context(
    document: &TaskDocument,
    task_id: &str,
    context_name: &str,
    cli_policy: &EnvironmentPolicy,
) -> Result<AppliedTaskContext, ContextError> {
    let context = resolve_task_context(document, task_id, context_name)?;
    let context_policy = context
        .environment
        .as_ref()
        .map_or(EnvironmentPolicy::Inherit, context_environment_to_policy);
    let spawn_env_set = context
        .environment
        .as_ref()
        .filter(|env| env.mode == ContextEnvironmentMode::Inherit)
        .map(|env| env.set.clone())
        .unwrap_or_default();
    let environment_policy = merge_environment_policies(cli_policy, &context_policy);
    let plan_secrets = plan_secret_entries(&context.secrets);

    Ok(AppliedTaskContext {
        context_name: context_name.to_owned(),
        environment_policy,
        spawn_env_set,
        plan_secrets,
        shell: context.shell.clone(),
        confirm: context.confirm,
    })
}

/// Resolve env-provider secrets for spawn using `lookup(ref) -> Option<value>`.
///
/// # Errors
///
/// Returns [`ContextError`] when a required secret is missing, delivery is unsupported,
/// or the provider is not `env`.
pub fn resolve_env_provider_secrets_with(
    entries: &[PlanSecretEntry],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, ContextError> {
    let mut resolved = BTreeMap::new();
    for entry in entries {
        if entry.provider != SecretProvider::Env {
            return Err(ContextError::UnsupportedProvider {
                slot: entry.name.clone(),
                reference: entry.reference.clone(),
                provider: entry.provider,
            });
        }
        let delivery = entry.delivery;
        match delivery {
            SecretDelivery::Env => {
                let value = lookup(&entry.reference).ok_or(ContextError::MissingSecret {
                    slot: entry.name.clone(),
                    reference: entry.reference.clone(),
                })?;
                resolved.insert(entry.name.clone(), value);
            }
            SecretDelivery::File | SecretDelivery::Stdin => {
                return Err(ContextError::UnsupportedDelivery {
                    slot: entry.name.clone(),
                    reference: entry.reference.clone(),
                    delivery,
                });
            }
        }
    }
    Ok(resolved)
}

/// Resolve env-provider secrets for spawn from the caller environment.
///
/// # Errors
///
/// Returns [`ContextError`] when a required secret is missing, delivery is unsupported,
/// or the provider is not `env`.
pub fn resolve_env_provider_secrets(
    entries: &[PlanSecretEntry],
) -> Result<BTreeMap<String, String>, ContextError> {
    resolve_env_provider_secrets_with(entries, |name| std::env::var(name).ok())
}

/// Prompt for context confirmation when required.
///
/// # Errors
///
/// Returns [`ContextError`] when confirmation is required but stdin is not a TTY,
/// the user declines, or stdin cannot be read.
pub fn enforce_context_confirm(
    context_name: &str,
    task_id: &str,
    confirm: bool,
) -> Result<(), ContextError> {
    if !confirm {
        return Ok(());
    }
    if assume_yes_enabled() {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(ContextError::ConfirmRequiredNonInteractive {
            context: context_name.to_owned(),
            task: task_id.to_owned(),
        });
    }
    let prompt = format!("Run task {task_id} with context {context_name}? [y/N] ");
    let mut stderr = io::stderr().lock();
    stderr
        .write_all(prompt.as_bytes())
        .map_err(|error| ContextError::ConfirmIo {
            message: error.to_string(),
        })?;
    stderr.flush().map_err(|error| ContextError::ConfirmIo {
        message: error.to_string(),
    })?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| ContextError::ConfirmIo {
            message: error.to_string(),
        })?;
    let normalized = answer.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(ContextError::ConfirmDeclined {
            context: context_name.to_owned(),
            task: task_id.to_owned(),
        })
    }
}

fn assume_yes_enabled() -> bool {
    match std::env::var(NXR_ASSUME_YES_ENV) {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !normalized.is_empty() && !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// Merge spawn-time env assignments (context `set` + resolved secrets) without logging values.
#[must_use]
pub fn merge_spawn_env_overrides(
    context_set: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = context_set.clone();
    merged.extend(secrets.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

/// Returns true when serialized JSON must not contain `needle` (secret redaction guard).
#[must_use]
pub fn serialized_plan_excludes_value(json: &str, needle: &str) -> bool {
    !json.contains(needle)
}

fn plan_secret_entries(secrets: &BTreeMap<String, ContextSecretRef>) -> Vec<PlanSecretEntry> {
    secrets
        .iter()
        .map(|(name, secret)| PlanSecretEntry {
            name: name.clone(),
            reference: secret.reference.clone(),
            delivery: secret_delivery_mode(secret),
            provider: secret_provider_mode(secret),
            value: PlanSecretValuePlaceholder::RUNTIME,
        })
        .collect()
}

fn context_environment_to_policy(environment: &ContextEnvironment) -> EnvironmentPolicy {
    match environment.mode {
        ContextEnvironmentMode::Inherit => EnvironmentPolicy::Inherit,
        ContextEnvironmentMode::Clean => EnvironmentPolicy::clean(
            environment.keep.clone(),
            environment.set.clone(),
            environment.unset.clone(),
        ),
    }
}

fn merge_environment_policies(
    cli: &EnvironmentPolicy,
    context: &EnvironmentPolicy,
) -> EnvironmentPolicy {
    match (cli, context) {
        (cli_policy, EnvironmentPolicy::Inherit) => cli_policy.clone(),
        (EnvironmentPolicy::Inherit, context_policy) => context_policy.clone(),
        (
            EnvironmentPolicy::Clean {
                keep: cli_keep,
                set: cli_set,
                unset: cli_unset,
            },
            EnvironmentPolicy::Clean {
                keep: ctx_keep,
                set: ctx_set,
                unset: ctx_unset,
            },
        ) => {
            let mut keep = cli_keep.clone();
            for name in ctx_keep {
                if !keep.contains(name) {
                    keep.push(name.clone());
                }
            }
            let mut set = cli_set.clone();
            set.extend(ctx_set.iter().map(|(k, v)| (k.clone(), v.clone())));
            let mut unset = cli_unset.clone();
            for name in ctx_unset {
                if !unset.contains(name) {
                    unset.push(name.clone());
                }
            }
            EnvironmentPolicy::Clean { keep, set, unset }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ContextSecretRef, ExecutionContext, TaskDefinition, TaskDocument};

    fn release_context() -> ExecutionContext {
        ExecutionContext {
            shell: Some("release".to_owned()),
            environment: Some(ContextEnvironment {
                mode: ContextEnvironmentMode::Inherit,
                keep: vec!["HOME".to_owned()],
                set: BTreeMap::from([("RELEASE_CHANNEL".to_owned(), "stable".to_owned())]),
                unset: Vec::new(),
            }),
            secrets: BTreeMap::from([(
                "DEPLOY_TOKEN".to_owned(),
                ContextSecretRef {
                    reference: "NXR_TEST_DEPLOY_TOKEN".to_owned(),
                    delivery: Some(SecretDelivery::Env),
                    provider: SecretProvider::Env,
                },
            )]),
            confirm: true,
        }
    }

    #[test]
    fn apply_task_context_builds_plan_secrets_without_values() {
        let mut document = TaskDocument::new(BTreeMap::new());
        document
            .contexts
            .insert("release".to_owned(), release_context());
        document.tasks.insert(
            "deploy".to_owned(),
            TaskDefinition {
                context: Some("release".to_owned()),
                ..TaskDefinition::new("deploy")
            },
        );

        let applied =
            apply_task_context(&document, "deploy", "release", &EnvironmentPolicy::Inherit)
                .expect("apply context");

        assert_eq!(applied.context_name, "release");
        assert_eq!(applied.shell.as_deref(), Some("release"));
        assert!(applied.confirm);
        assert_eq!(applied.plan_secrets.len(), 1);
        assert_eq!(applied.plan_secrets[0].name, "DEPLOY_TOKEN");
        assert_eq!(applied.plan_secrets[0].reference, "NXR_TEST_DEPLOY_TOKEN");
        assert_eq!(applied.plan_secrets[0].provider, SecretProvider::Env);
        assert_eq!(
            applied.plan_secrets[0].value,
            PlanSecretValuePlaceholder::RUNTIME
        );

        let plan_json = serde_json::to_string(&applied.plan_secrets).expect("serialize");
        assert!(serialized_plan_excludes_value(
            &plan_json,
            "super-secret-value"
        ));
    }

    #[test]
    fn resolve_env_provider_reads_caller_env_by_ref() {
        let entries = vec![PlanSecretEntry {
            name: "DEPLOY_TOKEN".to_owned(),
            reference: "NXR_TEST_DEPLOY_TOKEN".to_owned(),
            delivery: SecretDelivery::Env,
            provider: SecretProvider::Env,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }];
        let resolved = resolve_env_provider_secrets_with(&entries, |name| {
            if name == "NXR_TEST_DEPLOY_TOKEN" {
                Some("super-secret-value".to_owned())
            } else {
                None
            }
        })
        .expect("resolve");
        assert_eq!(
            resolved.get("DEPLOY_TOKEN").map(String::as_str),
            Some("super-secret-value")
        );
    }

    #[test]
    fn missing_secret_names_slot_and_ref() {
        let entries = vec![PlanSecretEntry {
            name: "DEPLOY_TOKEN".to_owned(),
            reference: "NXR_MISSING_SECRET_REF".to_owned(),
            delivery: SecretDelivery::Env,
            provider: SecretProvider::Env,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }];
        let error = resolve_env_provider_secrets_with(&entries, |_| None).expect_err("missing");
        assert!(matches!(error, ContextError::MissingSecret { .. }));
        assert!(error.to_string().contains("DEPLOY_TOKEN"));
        assert!(error.to_string().contains("NXR_MISSING_SECRET_REF"));
        assert!(!error.to_string().contains("super-secret"));
    }

    #[test]
    fn unsupported_file_delivery_errors() {
        let entries = vec![PlanSecretEntry {
            name: "KUBECONFIG".to_owned(),
            reference: "prod/kubeconfig".to_owned(),
            delivery: SecretDelivery::File,
            provider: SecretProvider::Env,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }];
        let error =
            resolve_env_provider_secrets_with(&entries, |_| Some("should-not-be-used".to_owned()))
                .expect_err("unsupported");
        assert!(matches!(error, ContextError::UnsupportedDelivery { .. }));
        assert!(error.to_string().contains("file"));
        assert!(error.to_string().contains("KUBECONFIG"));
    }

    #[test]
    fn unsupported_provider_errors_at_resolve_time() {
        let entries = vec![PlanSecretEntry {
            name: "DEPLOY_TOKEN".to_owned(),
            reference: "openseat/prod/token".to_owned(),
            delivery: SecretDelivery::Env,
            provider: SecretProvider::Sops,
            value: PlanSecretValuePlaceholder::RUNTIME,
        }];
        let error = resolve_env_provider_secrets_with(&entries, |_| Some("ignored".to_owned()))
            .expect_err("unsupported provider");
        assert!(matches!(error, ContextError::UnsupportedProvider { .. }));
        assert!(error.to_string().contains("sops"));
        assert!(error.to_string().contains("DEPLOY_TOKEN"));
    }

    #[test]
    fn default_delivery_is_env() {
        let secret = ContextSecretRef {
            reference: "TOKEN".to_owned(),
            delivery: None,
            provider: SecretProvider::Env,
        };
        assert_eq!(secret_delivery_mode(&secret), SecretDelivery::Env);
        assert_eq!(secret_provider_mode(&secret), SecretProvider::Env);
    }

    #[test]
    fn plan_secret_entry_serializes_runtime_placeholder() {
        let entry = PlanSecretEntry {
            name: "DEPLOY_TOKEN".to_owned(),
            reference: "NXR_TEST_DEPLOY_TOKEN".to_owned(),
            delivery: SecretDelivery::Env,
            provider: SecretProvider::Env,
            value: PlanSecretValuePlaceholder::RUNTIME,
        };
        let value = serde_json::to_value(&entry).expect("serialize");
        assert_eq!(value["value"], "<runtime>");
        assert_eq!(value["ref"], "NXR_TEST_DEPLOY_TOKEN");
        assert_eq!(value["provider"], "env");
    }

    #[test]
    fn clean_context_merges_with_cli_clean_policy() {
        let context = ContextEnvironment {
            mode: ContextEnvironmentMode::Clean,
            keep: vec!["SSH_AUTH_SOCK".to_owned()],
            set: BTreeMap::from([("RELEASE_CHANNEL".to_owned(), "stable".to_owned())]),
            unset: Vec::new(),
        };
        let cli = EnvironmentPolicy::clean(["HOME".to_owned()], [], []);
        let merged = merge_environment_policies(&cli, &context_environment_to_policy(&context));
        let EnvironmentPolicy::Clean { keep, set, .. } = merged else {
            panic!("expected clean policy");
        };
        assert!(keep.contains(&"HOME".to_owned()));
        assert!(keep.contains(&"SSH_AUTH_SOCK".to_owned()));
        assert_eq!(
            set.get("RELEASE_CHANNEL").map(String::as_str),
            Some("stable")
        );
    }

    #[test]
    fn unknown_context_is_actionable() {
        let document = TaskDocument::new(BTreeMap::new());
        let error = apply_task_context(&document, "deploy", "release", &EnvironmentPolicy::Inherit)
            .expect_err("unknown");
        assert!(matches!(error, ContextError::UnknownContext { .. }));
    }
}
