//! `nxr cache` subcommands.

use std::io::{self, Write};

use nxr_completion::{
    DiscoveryCacheOptions, clear_discovery_cache, discovery_cache_status, explain_discovery_cache,
    gc_discovery_cache, invalidate_discovery_cache,
};
use nxr_core::cas::CacheExplain;
use nxr_core::cas::{clear_workspace_cas, workspace_cas_status};
use nxr_core::diagnostics::exit;
use nxr_core::{
    DAEMON_MAX_CACHE_ENTRIES_PER_MAP, DaemonStatus, action_digest_index_status,
    clear_action_digest_index, clear_dev_env_cache, clear_merkle_index, clear_plan_cache,
    clear_store_exe_cache, daemon_socket_path, dev_env_cache_status, gc_dev_env_cache,
    gc_plan_cache, invalidate_dev_env_cache, invalidate_plan_cache, merkle_index_status,
    plan_cache_status, store_exe_cache_status, try_connect, try_daemon_invalidate_dev_env,
    try_daemon_invalidate_discovery, try_daemon_invalidate_plan,
};
use nxr_nix::{
    OptionalNixFlags, capability_cache_status, clear_capability_cache, plan_discovery_eval,
};
use nxr_task::{PlanError, build_execution_plan_roots, resolve_task_name};
use serde::Serialize;

use crate::commands::common::{PrepareError, WorkspaceSnapshot, WorkspaceState};
use crate::commands::workspace_cache::explain_workspace_cache;
use crate::flake::{FlakeResolveError, resolve_flake};
use crate::runner_output::RunnerOutput;

/// Errors while managing the discovery cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Nix(#[from] nxr_nix::NixError),
    #[error("task {name} not found")]
    TaskNotFound { name: String },
}

impl CacheError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
            Self::Flake(error) => error.exit_code(),
            Self::Prepare(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Plan(_) | Self::TaskNotFound { .. } => exit::NOT_FOUND,
        }
    }
}

#[derive(Serialize)]
struct CacheClearJson {
    discovery_removed: usize,
    capabilities_removed: usize,
    workspace_removed: usize,
    plans_removed: usize,
    dev_env_removed: usize,
    store_exe_removed: usize,
    action_digests_removed: usize,
    merkle_removed: usize,
}

#[derive(Clone, Serialize)]
struct DaemonCacheLayer {
    available: bool,
    entries: usize,
    max_entries: usize,
}

#[derive(Serialize)]
struct WarmCacheStatusSection {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_secs: Option<u64>,
    entries: usize,
    total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    oldest_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newest_age_secs: Option<u64>,
    daemon: DaemonCacheLayer,
}

#[derive(Serialize)]
struct CacheStatusSection {
    path: String,
    entries: usize,
    total_bytes: u64,
}

#[derive(Serialize)]
struct CacheStatusJson {
    discovery: WarmCacheStatusSection,
    capabilities: CacheStatusSection,
    workspace: CacheStatusSection,
    plans: WarmCacheStatusSection,
    dev_env: WarmCacheStatusSection,
    store_exe: CacheStatusSection,
    action_digests: CacheStatusSection,
    merkle: CacheStatusSection,
}

#[derive(Serialize)]
struct CacheGcJson {
    discovery_pruned: usize,
    plans_pruned: usize,
    dev_env_pruned: usize,
}

#[derive(Serialize)]
struct CacheInvalidateJson {
    disk_removed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_invalidated: Option<usize>,
}

/// Remove all discovery cache entries.
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn clear(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery_removed = clear_discovery_cache()?;
    let _ = try_daemon_invalidate_discovery(None);
    let capabilities_removed = clear_capability_cache()?;
    let workspace_removed = clear_workspace_cas()?;
    let plans_removed = clear_plan_cache()?;
    let _ = try_daemon_invalidate_plan(None);
    let dev_env_removed = clear_dev_env_cache()?;
    let _ = try_daemon_invalidate_dev_env(None);
    let store_exe_removed = clear_store_exe_cache()?;
    let action_digests_removed = clear_action_digest_index()?;
    let merkle_removed = clear_merkle_index()?;
    if json {
        let payload = CacheClearJson {
            discovery_removed,
            capabilities_removed,
            workspace_removed,
            plans_removed,
            dev_env_removed,
            store_exe_removed,
            action_digests_removed,
            merkle_removed,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        runner
            .info(format!(
                "removed {discovery_removed} discovery cache entr{}, {capabilities_removed} capability cache entr{}, {workspace_removed} workspace CAS entr{}, {plans_removed} prepared-plan cache entr{}, {dev_env_removed} dev-environment cache entr{}, {store_exe_removed} store-exe cache entr{}, {action_digests_removed} action-digest index entr{}, and {merkle_removed} merkle index entr{}",
                if discovery_removed == 1 { "y" } else { "ies" },
                if capabilities_removed == 1 { "y" } else { "ies" },
                if workspace_removed == 1 { "y" } else { "ies" },
                if plans_removed == 1 { "y" } else { "ies" },
                if dev_env_removed == 1 { "y" } else { "ies" },
                if store_exe_removed == 1 { "y" } else { "ies" },
                if action_digests_removed == 1 { "y" } else { "ies" },
                if merkle_removed == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}

/// Prune TTL-expired discovery, prepared-plan, and dev-environment disk cache entries.
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn gc(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery_pruned = gc_discovery_cache()?;
    let plans_pruned = gc_plan_cache()?;
    let dev_env_pruned = gc_dev_env_cache()?;
    if json {
        let payload = CacheGcJson {
            discovery_pruned,
            plans_pruned,
            dev_env_pruned,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        runner
            .info(format!(
                "pruned {discovery_pruned} discovery, {plans_pruned} prepared-plan, and {dev_env_pruned} dev-environment cache entr{} past TTL",
                if discovery_pruned + plans_pruned + dev_env_pruned == 1 {
                    "y"
                } else {
                    "ies"
                },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}

/// Invalidate discovery, prepared-plan, or dev-environment caches (disk + best-effort daemon).
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn invalidate_discovery(
    file_stem: Option<&str>,
    daemon_key: Option<&str>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let disk_removed = invalidate_discovery_cache(file_stem)?;
    let daemon_invalidated = try_daemon_invalidate_discovery(daemon_key);
    emit_invalidate_result("discovery", disk_removed, daemon_invalidated, json, runner)
}

/// Invalidate prepared-plan caches (disk + best-effort daemon).
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn invalidate_plan(
    key_digest: Option<&str>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let disk_removed = invalidate_plan_cache(key_digest)?;
    let daemon_invalidated = try_daemon_invalidate_plan(key_digest);
    emit_invalidate_result(
        "prepared-plan",
        disk_removed,
        daemon_invalidated,
        json,
        runner,
    )
}

/// Invalidate dev-environment caches (disk + best-effort daemon).
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn invalidate_dev_env(
    key_digest: Option<&str>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let disk_removed = invalidate_dev_env_cache(key_digest)?;
    let daemon_invalidated = try_daemon_invalidate_dev_env(key_digest);
    emit_invalidate_result(
        "dev-environment",
        disk_removed,
        daemon_invalidated,
        json,
        runner,
    )
}

fn emit_invalidate_result(
    label: &str,
    disk_removed: usize,
    daemon_invalidated: Option<usize>,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    if json {
        let payload = CacheInvalidateJson {
            disk_removed,
            daemon_invalidated,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
        return Ok(());
    }

    let daemon_suffix = match daemon_invalidated {
        Some(count) => format!(
            ", {count} nxrd entr{} invalidated",
            if count == 1 { "y" } else { "ies" }
        ),
        None => String::new(),
    };
    runner
        .info(format!(
            "invalidated {label} cache: {disk_removed} disk entr{}{daemon_suffix}",
            if disk_removed == 1 { "y" } else { "ies" },
        ))
        .map_err(CacheError::Io)
}

/// Print discovery cache location and size.
///
/// # Errors
///
/// Returns [`CacheError`] when the cache directory cannot be read or output fails.
pub fn status(json: bool, mut runner: RunnerOutput) -> Result<(), CacheError> {
    let daemon = daemon_warm_layers();
    let discovery = discovery_cache_status()?;
    let capabilities = capability_cache_status()?;
    let workspace = workspace_cas_status()?;
    let plans = plan_cache_status()?;
    let dev_env = dev_env_cache_status()?;
    let store_exe = store_exe_cache_status()?;
    let action_digests = action_digest_index_status()?;
    let merkle = merkle_index_status()?;
    if json {
        let payload = CacheStatusJson {
            discovery: warm_section(
                &discovery.path,
                None,
                discovery.ttl_secs,
                discovery.entries,
                discovery.total_bytes,
                discovery.oldest_age_secs,
                discovery.newest_age_secs,
                daemon.discovery,
            ),
            capabilities: CacheStatusSection {
                path: capabilities.path,
                entries: capabilities.entries,
                total_bytes: capabilities.total_bytes,
            },
            workspace: CacheStatusSection {
                path: workspace.path,
                entries: workspace.entries,
                total_bytes: workspace.total_bytes,
            },
            plans: warm_section(
                &plans.path,
                Some(plans.enabled),
                plans.ttl_secs,
                plans.entries,
                plans.total_bytes,
                plans.oldest_age_secs,
                plans.newest_age_secs,
                daemon.plans,
            ),
            dev_env: warm_section(
                &dev_env.path,
                Some(dev_env.enabled),
                dev_env.ttl_secs,
                dev_env.entries,
                dev_env.total_bytes,
                dev_env.oldest_age_secs,
                dev_env.newest_age_secs,
                daemon.dev_env,
            ),
            store_exe: CacheStatusSection {
                path: store_exe.path,
                entries: store_exe.entries,
                total_bytes: store_exe.total_bytes,
            },
            action_digests: CacheStatusSection {
                path: action_digests.path.display().to_string(),
                entries: action_digests.entries,
                total_bytes: action_digests.total_bytes,
            },
            merkle: CacheStatusSection {
                path: merkle.path.display().to_string(),
                entries: merkle.entries,
                total_bytes: merkle.total_bytes,
            },
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        render_warm_status_section(
            &mut runner,
            "discovery",
            &discovery.path,
            None,
            discovery.ttl_secs,
            discovery.entries,
            discovery.total_bytes,
            discovery.oldest_age_secs,
            discovery.newest_age_secs,
            daemon.discovery,
        )?;
        render_status_section(
            &mut runner,
            "capability",
            &capabilities.path,
            capabilities.entries,
            capabilities.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "workspace",
            &workspace.path,
            workspace.entries,
            workspace.total_bytes,
        )?;
        render_warm_status_section(
            &mut runner,
            "prepared-plan",
            &plans.path,
            Some(plans.enabled),
            plans.ttl_secs,
            plans.entries,
            plans.total_bytes,
            plans.oldest_age_secs,
            plans.newest_age_secs,
            daemon.plans,
        )?;
        render_warm_status_section(
            &mut runner,
            "dev-environment",
            &dev_env.path,
            Some(dev_env.enabled),
            dev_env.ttl_secs,
            dev_env.entries,
            dev_env.total_bytes,
            dev_env.oldest_age_secs,
            dev_env.newest_age_secs,
            daemon.dev_env,
        )?;
        render_status_section(
            &mut runner,
            "store-exe",
            &store_exe.path,
            store_exe.entries,
            store_exe.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "action-digests",
            &action_digests.path.display().to_string(),
            action_digests.entries,
            action_digests.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "merkle-index",
            &merkle.path.display().to_string(),
            merkle.entries,
            merkle.total_bytes,
        )?;
    }
    Ok(())
}

struct DaemonWarmLayers {
    discovery: DaemonCacheLayer,
    plans: DaemonCacheLayer,
    dev_env: DaemonCacheLayer,
}

fn daemon_warm_layers() -> DaemonWarmLayers {
    let unavailable = DaemonCacheLayer {
        available: false,
        entries: 0,
        max_entries: DAEMON_MAX_CACHE_ENTRIES_PER_MAP,
    };
    let Ok(mut conn) = try_connect(&daemon_socket_path()) else {
        return DaemonWarmLayers {
            discovery: unavailable.clone(),
            plans: unavailable.clone(),
            dev_env: unavailable,
        };
    };
    let Ok(status) = conn.call::<DaemonStatus>("status", None) else {
        return DaemonWarmLayers {
            discovery: unavailable.clone(),
            plans: unavailable.clone(),
            dev_env: unavailable,
        };
    };
    DaemonWarmLayers {
        discovery: daemon_layer(true, status.discovery_entries),
        plans: daemon_layer(true, status.plan_entries),
        dev_env: daemon_layer(true, status.dev_env_entries),
    }
}

fn daemon_layer(available: bool, entries: usize) -> DaemonCacheLayer {
    DaemonCacheLayer {
        available,
        entries,
        max_entries: DAEMON_MAX_CACHE_ENTRIES_PER_MAP,
    }
}

fn warm_section(
    path: &str,
    enabled: Option<bool>,
    ttl_secs: Option<u64>,
    entries: usize,
    total_bytes: u64,
    oldest_age_secs: Option<u64>,
    newest_age_secs: Option<u64>,
    daemon: DaemonCacheLayer,
) -> WarmCacheStatusSection {
    WarmCacheStatusSection {
        path: path.to_owned(),
        enabled,
        ttl_secs,
        entries,
        total_bytes,
        oldest_age_secs,
        newest_age_secs,
        daemon,
    }
}

/// Explain workspace CAS key material and hit/miss for a task.
///
/// # Errors
///
/// Returns [`CacheError`] when discovery, planning, or output fails.
pub fn explain_task(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    task: &str,
    json: bool,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let snapshot = WorkspaceSnapshot::load(flake_arg, nix_override, true, &Default::default())?;
    let document = snapshot
        .tasks
        .as_ref()
        .ok_or_else(|| CacheError::TaskNotFound {
            name: task.to_owned(),
        })?;
    let canonical = resolve_task_name(document, task)
        .map_err(|error| CacheError::TaskNotFound { name: error.name })?
        .to_owned();
    let plan = build_execution_plan_roots(
        &document.tasks,
        &[canonical.as_str()],
        nxr_task::FailurePolicy::FailFast,
        None,
    )?;
    snapshot
        .validate_task_apps(document)
        .map_err(PrepareError::NotFound)?;
    let prepared = snapshot.prepare_task_nodes(
        document,
        std::slice::from_ref(&canonical),
        &plan.serial_order,
        &[],
        false,
        None,
        None,
        crate::shell_mode::ShellMode::Smart,
        &nxr_core::EnvironmentPolicy::Inherit,
        &Default::default(),
        None,
    )?;
    let node = prepared
        .get(&canonical)
        .ok_or_else(|| CacheError::TaskNotFound {
            name: canonical.clone(),
        })?;
    let explain = explain_workspace_cache(node);
    if json {
        let rendered = serde_json::to_string_pretty(&explain)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        render_cache_explain(&explain, &canonical, runner)?;
    }
    Ok(())
}

fn render_cache_explain(
    explain: &CacheExplain,
    task: &str,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    runner
        .info(format!("cache explain for task {task}"))
        .map_err(CacheError::Io)?;
    runner
        .info(format!("  tier: {:?}", explain.tier))
        .map_err(CacheError::Io)?;
    runner
        .info(format!("  cache_enabled: {}", explain.cache_enabled))
        .map_err(CacheError::Io)?;
    if let Some(key) = &explain.action_key {
        runner
            .info(format!("  action_key: {key}"))
            .map_err(CacheError::Io)?;
    }
    runner
        .info(format!("  lookup: {:?}", explain.lookup))
        .map_err(CacheError::Io)?;
    if !explain.key_components.is_empty() {
        runner.info("  key_components:").map_err(CacheError::Io)?;
        for (label, value) in &explain.key_components {
            runner
                .info(format!("    {label}: {value}"))
                .map_err(CacheError::Io)?;
        }
    }
    Ok(())
}

/// Explain discovery cache validity and miss reasons for the selected flake.
///
/// # Errors
///
/// Returns [`CacheError`] when flake resolution, cache inspection, or output fails.
pub fn explain(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
    require_tasks: bool,
    json: bool,
    nix_flags: &OptionalNixFlags,
    runner: RunnerOutput,
) -> Result<(), CacheError> {
    let invocation_directory =
        crate::commands::common::current_invocation_directory().map_err(CacheError::Prepare)?;
    let _flake = resolve_flake(flake_arg, &invocation_directory).map_err(CacheError::Flake)?;
    let mut state = WorkspaceState::new(flake_arg, nix_override, nix_flags);
    let context = state.discovery_context().map_err(CacheError::Prepare)?;
    let adapter = state.adapter().map_err(CacheError::Nix)?;
    let eval_plan = plan_discovery_eval(
        &adapter.version_banner,
        adapter.config_json.as_deref(),
        require_tasks,
    );
    let report = explain_discovery_cache(
        &context,
        DiscoveryCacheOptions {
            refresh: false,
            require_tasks,
        },
        eval_plan.use_coalesced_discovery,
        eval_plan.strategy,
    )?;

    if json {
        let rendered = serde_json::to_string_pretty(&report)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
        return Ok(());
    }

    let entry = &report.entry;
    if !entry.available {
        runner
            .info("discovery cache unavailable (remote flake or host has no cache directory)")
            .map_err(CacheError::Io)?;
        return Ok(());
    }

    runner
        .info(format!(
            "discovery cache: hit={} path={}",
            entry.hit,
            entry.cache_file.as_deref().unwrap_or("n/a")
        ))
        .map_err(CacheError::Io)?;
    if let Some(key) = &entry.invalidation_key {
        runner
            .info(format!("invalidation_key: {key}"))
            .map_err(CacheError::Io)?;
    }
    if let Some(key) = &entry.cached_invalidation_key {
        runner
            .info(format!("cached_invalidation_key: {key}"))
            .map_err(CacheError::Io)?;
    }
    runner
        .info(format!(
            "discovery_eval_strategy: {}",
            serde_json::to_string(&report.discovery_eval_strategy).map_err(CacheError::Json)?
        ))
        .map_err(CacheError::Io)?;
    runner
        .info(format!(
            "coalesced_discovery_available: {}",
            report.coalesced_discovery_available
        ))
        .map_err(CacheError::Io)?;
    for reason in &entry.miss_reasons {
        let rendered = serde_json::to_string(reason).map_err(CacheError::Json)?;
        runner
            .info(format!("miss_reason: {rendered}"))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}

fn render_warm_status_section(
    runner: &mut RunnerOutput,
    label: &str,
    path: &str,
    enabled: Option<bool>,
    ttl_secs: Option<u64>,
    entries: usize,
    total_bytes: u64,
    oldest_age_secs: Option<u64>,
    newest_age_secs: Option<u64>,
    daemon: DaemonCacheLayer,
) -> Result<(), CacheError> {
    if path.is_empty() {
        runner
            .info(format!("{label} cache unavailable on this host"))
            .map_err(CacheError::Io)?;
        return Ok(());
    }

    let enabled_suffix = enabled
        .map(|enabled| format!(", enabled={enabled}"))
        .unwrap_or_default();
    let ttl_suffix = ttl_secs
        .map(|ttl| format!(", ttl_secs={ttl}"))
        .unwrap_or_else(|| ", ttl_secs=off".to_owned());
    let age_suffix = match (oldest_age_secs, newest_age_secs) {
        (Some(oldest), Some(newest)) => {
            format!(", oldest_age_secs={oldest}, newest_age_secs={newest}")
        }
        _ => String::new(),
    };
    let daemon_suffix = if daemon.available {
        format!(", nxrd={} (max {})", daemon.entries, daemon.max_entries)
    } else {
        ", nxrd=absent".to_owned()
    };
    runner
        .info(format!(
            "{label} cache: {path} ({entries} entr{}, {total_bytes} bytes{enabled_suffix}{ttl_suffix}{age_suffix}{daemon_suffix})",
            if entries == 1 { "y" } else { "ies" },
        ))
        .map_err(CacheError::Io)
}

fn render_status_section(
    runner: &mut RunnerOutput,
    label: &str,
    path: &str,
    entries: usize,
    total_bytes: u64,
) -> Result<(), CacheError> {
    if path.is_empty() {
        runner
            .info(format!("{label} cache unavailable on this host"))
            .map_err(CacheError::Io)?;
    } else {
        runner
            .info(format!(
                "{label} cache: {path} ({entries} entr{}, {total_bytes} bytes)",
                if entries == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}
