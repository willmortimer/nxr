//! Nix executable discovery, capability detection, and app resolution.

pub mod adapter;
pub mod capabilities;
pub mod capability_cache;
pub mod coalesce;
pub mod command;
pub mod configurations;
pub mod determinate;
pub mod discovery;
pub mod inventory;
pub mod inventory_list;
pub mod resolve;
pub mod store_exe;
pub mod store_query;
pub mod strategy;
pub mod suggest;
pub mod tasks;

use camino::Utf8PathBuf;
use nxr_core::sanitize::sanitize_terminal_text;

pub use adapter::NixAdapter;
pub use capabilities::{
    CapabilityEvidence, CapabilityProvenance, FlagPolicy, NixCapabilities, NixDistribution,
    NixFailureKind, NixVersion, OptionalNixFlags, TESTED_NIX_SUPPORT_FLOOR, config_bool_setting,
    config_string_list_setting, detect_capabilities, detect_system, locate_nix,
    negotiate_capabilities, parse_nix_distribution, parse_nix_version_output, probe_config_json,
    probe_version_banner, run_nix,
};
pub use capability_cache::{
    CAPABILITY_CACHE_ENV, CAPABILITY_CACHE_TTL_ENV, CapabilityCacheStatus, capability_cache_dir,
    capability_cache_enabled, capability_cache_status, clear_capability_cache,
    detect_nix_environment,
};
pub use coalesce::{
    CoalescedDiscovery, CoalescedDiscoveryError, CoalescedWorkspace, coalesced_discovery_args,
    coalesced_discovery_available, coalesced_discovery_expr, discover_coalesced,
};
pub use command::{
    NIX_EXECUTABLE_ENV, attr_installable, check_installable, current_system_args,
    flake_app_program_eval_args, flake_eval_json_args, flake_show_args, nix_build_args,
    nix_build_no_link_print_out_paths_args, nix_develop_args, nix_develop_wrap_run_args,
    nix_flake_check_args, nix_fmt_args, nix_run_args, package_installable,
    token_is_explicit_installable,
};
pub use configurations::{
    ConfigurationEntry, ConfigurationKind, configuration_installable, find_configuration,
    is_configuration_output_key, list_configurations, parse_configuration_output_node,
};
pub use determinate::{
    DeterminatePerformanceFeatures, DeterminateWasmSupport, LazyTreesState, NixdProbe,
    distribution_from_version_banner, effective_experimental_features, host_is_macos,
    probe_ci_environment, probe_nixd, probe_performance_features, probe_wasm_support,
    redact_sensitive_text,
};
pub use discovery::{
    OutputTable, discover_apps, discover_outputs_with_args, flake_show_has_nxr_for_system,
    parse_apps_from_flake_show, parse_outputs_from_flake_show,
};
pub use inventory::{
    FlakeInventory, InventoryNode, StandardOutputEntry, StandardOutputKind, list_standard_outputs,
    parse_flake_inventory,
};
pub use inventory_list::{
    InventoryEntry, InventoryRole, list_inventory_entries, list_inventory_roles,
};
pub use resolve::{
    AppNotFoundError, OutputNotFoundError, resolve_app_by_name, resolve_output_by_name,
};
pub use store_exe::{RealisedAppProgram, realise_flake_app_program};
pub use store_query::{
    FORCE_FS_STORE_QUERIES_ENV, StorePathInfo, batched_store_queries_enabled,
    batched_store_queries_enabled_for_nix, prefer_batched_store_queries, query_store_paths,
    store_exe_paths_usable, store_path_registered,
};
pub use strategy::{
    DiscoveryEvalPlan, DiscoveryEvalStrategy, FORCE_COALESCED_DISCOVERY_ENV,
    FORCE_COMPATIBILITY_STRATEGY_ENV, plan_discovery_eval,
};
pub use suggest::{DEFAULT_SUGGESTION_LIMIT, rank_app_suggestions, rank_name_suggestions};
pub use tasks::{TaskDiscoveryError, discover_tasks, parse_task_document, tasks_attr_path};

/// Errors from the Nix adapter boundary.
#[derive(Debug)]
pub enum NixError {
    /// `nix` was not found at the expected location.
    NixNotFound { path: Utf8PathBuf },

    /// Failed to spawn `nix`.
    SpawnFailed {
        nix: Utf8PathBuf,
        source: std::io::Error,
    },

    /// `builtins.currentSystem` returned unusable output.
    InvalidSystemOutput,

    /// `nix --version` returned unusable output.
    InvalidVersionOutput,

    /// Flakes are required but not enabled in the Nix configuration.
    FlakesDisabled { version: NixVersion },

    /// A user-requested optional Nix flag is unsupported on this Nix version.
    UnsupportedOptionalFlag {
        flag: &'static str,
        version: NixVersion,
    },

    /// A `nix` subprocess exited unsuccessfully.
    CommandFailed {
        nix: Utf8PathBuf,
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
        kind: NixFailureKind,
    },

    /// `nix` stdout was not valid JSON.
    InvalidJson { source: serde_json::Error },

    /// Flake show JSON could not be normalized into apps.
    ParseApps(ParseAppsError),
}

impl std::error::Error for NixError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpawnFailed { source, .. } => Some(source),
            Self::InvalidJson { source } => Some(source),
            Self::ParseApps(error) => Some(error),
            Self::NixNotFound { .. }
            | Self::InvalidSystemOutput
            | Self::InvalidVersionOutput
            | Self::FlakesDisabled { .. }
            | Self::UnsupportedOptionalFlag { .. }
            | Self::CommandFailed { .. } => None,
        }
    }
}

impl From<ParseAppsError> for NixError {
    fn from(error: ParseAppsError) -> Self {
        Self::ParseApps(error)
    }
}

impl std::fmt::Display for NixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl NixError {
    /// User-facing message with command context and sanitized subprocess output.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NixNotFound { path } => {
                format!(
                    "nix executable not found at `{}` (set {} or ensure `nix` is on PATH)",
                    path,
                    command::NIX_EXECUTABLE_ENV
                )
            }
            Self::SpawnFailed { nix, source } => {
                format!("failed to run `{nix}`: {source}")
            }
            Self::InvalidSystemOutput => {
                "nix returned an invalid current system string (try `nix eval --raw --impure --expr builtins.currentSystem`)"
                    .to_owned()
            }
            Self::InvalidVersionOutput => {
                "nix returned an invalid version string (try `nix --version`)"
                    .to_owned()
            }
            Self::FlakesDisabled { version } => {
                format!(
                    "Nix {version} does not have flakes enabled (`experimental-features` lacks \
                     `flakes`). Enable flakes, for example: \
                     `mkdir -p ~/.config/nix && echo 'experimental-features = nix-command flakes' \
                     >> ~/.config/nix/nix.conf`"
                )
            }
            Self::UnsupportedOptionalFlag { flag, version } => {
                format!(
                    "Nix {version} does not support `{flag}` (requested via nxr); upgrade Nix or \
                     omit the flag"
                )
            }
            Self::CommandFailed {
                nix,
                args,
                status,
                stderr,
                kind,
            } => {
                let action = match kind {
                    NixFailureKind::Capability => "detect Nix capabilities",
                    NixFailureKind::Evaluation => "evaluate flake",
                };
                let command = format_nix_invocation(nix, args);
                let status = status
                    .map_or_else(|| "exited with an unknown status".to_owned(), |code| {
                        format!("exited with status {code}")
                    });
                let detail = sanitize_terminal_text(stderr.trim());
                let detail = if detail.is_empty() {
                    "no stderr output".to_owned()
                } else {
                    detail
                };

                format!("failed to {action} (`{command}`; {status}): {detail}")
            }
            Self::InvalidJson { source } => {
                format!("nix output was not valid JSON: {source}")
            }
            Self::ParseApps(error) => error.to_string(),
        }
    }

    /// Stable `nxr` exit code for this adapter error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;

        match self {
            Self::NixNotFound { .. }
            | Self::SpawnFailed { .. }
            | Self::InvalidSystemOutput
            | Self::InvalidVersionOutput
            | Self::FlakesDisabled { .. }
            | Self::UnsupportedOptionalFlag { .. }
            | Self::CommandFailed {
                kind: NixFailureKind::Capability,
                ..
            } => exit::NIX_CAPABILITY,
            Self::CommandFailed {
                kind: NixFailureKind::Evaluation,
                ..
            }
            | Self::InvalidJson { .. }
            | Self::ParseApps { .. } => exit::EVALUATION,
        }
    }
}

fn format_nix_invocation(nix: &Utf8PathBuf, args: &[String]) -> String {
    let mut command = nix.as_str().to_owned();
    for arg in args {
        if arg.contains(char::is_whitespace) {
            command.push(' ');
            command.push('"');
            command.push_str(arg);
            command.push('"');
        } else {
            command.push(' ');
            command.push_str(arg);
        }
    }
    command
}

/// Errors while parsing app metadata from flake show JSON.
#[derive(Debug, thiserror::Error)]
pub enum ParseAppsError {
    /// Reserved for future structured parse failures.
    #[error("failed to parse apps from flake show output")]
    InvalidStructure,
}
