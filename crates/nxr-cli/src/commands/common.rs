//! Shared helpers for list / run / plan commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_completion::{
    discovery_inputs_fingerprint, hint_discovery_inputs_for_root, nix_tree_fingerprint,
};
use nxr_core::PlanPrepareGuard;
use nxr_core::diagnostics::exit;
use nxr_core::{
    App, EnvironmentPolicy, Plan, PlanCacheKeyMaterial, PlanCacheSharedFingerprints, PlanCommand,
    PlanKind, PlanPrepareKind, PlanSecretRef, RunDigestCache, daemon_plan_entry,
    daemon_plan_to_hit, digest_environment_policy, digest_nix_flags, flake_lock_digest,
    git_source_identity, lookup_prepared_plan, plan_cache_enabled, plan_cache_key_digest,
    record_node_prepared, record_plan_cache_hit, record_plan_cache_miss,
    record_spawn_plan_cancelled, record_spawn_plan_prepared, store_prepared_plan, try_once,
};
use nxr_nix::{
    AppNotFoundError, NixAdapter, NixCapabilities, NixError, OptionalNixFlags, OutputTable,
    TESTED_NIX_SUPPORT_FLOOR, detect_capabilities, flake_show_has_nxr_for_system, locate_nix,
    nix_develop_wrap_run_args, nix_run_args, parse_apps_from_flake_show,
    parse_outputs_from_flake_show, resolve_app_by_name,
};
use nxr_task::{
    ContextError, ExecutionPlan, PlanSecretEntry, SchemaError, SecretDelivery, TaskDocument,
    WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION, WorkspaceCachePlan,
    WorkspaceCachePlanOptions, apply_task_context, build_workspace_cache_plan, parameter_names,
};
use nxr_watch::PrewarmContext;

use crate::commands::dev_env::{
    ENV_MODE_PROCESS, ENV_MODE_SHELL, materialize_develop_shell_policy,
    materialize_process_shell_policy,
};
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::shell_mode::{
    ShellMode, active_dev_shell, effective_shell_wrap, resolve_effective_shell,
    strip_nix_develop_wrap,
};

/// Inputs shared by `run`, bare-app, and `plan` preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub app: &'a str,
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_mode: ShellMode,
    pub environment_policy: EnvironmentPolicy,
    pub nix_flags: &'a OptionalNixFlags,
}

/// Inputs for flake discovery without a resolved app target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoverRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub nix_flags: &'a OptionalNixFlags,
}

/// Discovered apps for a selected flake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredApps {
    pub apps: Vec<App>,
}

/// Prepared execution plan for an app target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedPlan {
    pub plan: Plan,
    pub nix: Utf8PathBuf,
    pub execution_directory: Utf8PathBuf,
    /// Local flake root when the flake is path-based (store-exe / plan cache).
    pub local_root: Option<Utf8PathBuf>,
}

/// Environment variable forcing eager full-DAG prepare (`off` / `0` / `false` / `no`).
///
/// Default (unset or any other value): staged / lazy prepare (ADR-0158).
pub const LAZY_PREP_ENV: &str = "NXR_LAZY_PREP";

/// Environment variable disabling CAS‖SpawnPlan pipelining (`off` / `0` / `false` / `no`).
///
/// Default (unset or any other value): overlap CAS lookup with SpawnPlan prep on
/// live lazy runs (ADR-0159). When off, stages fuse and CAS runs only after a
/// full spawn plan is ready (Wave 4b serial behavior).
pub const CAS_PLAN_PIPELINE_ENV: &str = "NXR_CAS_PLAN_PIPELINE";

/// Whether staged / lazy node preparation is enabled (default on).
#[must_use]
pub fn lazy_prep_enabled() -> bool {
    lazy_prep_enabled_for_env(std::env::var(LAZY_PREP_ENV).ok().as_deref())
}

fn lazy_prep_enabled_for_env(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Whether CAS lookup may overlap SpawnPlan preparation (default on).
#[must_use]
pub fn cas_plan_pipeline_enabled() -> bool {
    cas_plan_pipeline_enabled_for_env(std::env::var(CAS_PLAN_PIPELINE_ENV).ok().as_deref())
}

fn cas_plan_pipeline_enabled_for_env(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Preparation stage for CAS‖plan pipelining (ADR-0159).
///
/// [`NodePrepStage::CasInputs`] finishes action-key / digest work without requiring
/// a finalized spawn argv. [`NodePrepStage::SpawnPlan`] builds nix argv (and marks
/// the node spawn-ready). Callers request CasInputs before CAS restore and
/// SpawnPlan before spawn.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NodePrepStage {
    /// Action key / workspace CAS plan inputs (no finalized spawn argv yet).
    CasInputs,
    /// Full spawn argv (`build_plan`) ready for child launch.
    SpawnPlan,
}

/// Precomputed spawn inputs for one task graph node.
///
/// Built from a [`WorkspaceSnapshot`] when the node approaches execution (lazy)
/// or eagerly for dry-run / watch reuse / `NXR_LAZY_PREP=off`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedTaskNode {
    pub id: String,
    pub program: Utf8PathBuf,
    pub arguments: Vec<String>,
    pub cwd: Utf8PathBuf,
    pub environment: EnvironmentPolicy,
    /// Full app plan (dry-run / JSON rendering).
    pub plan: Plan,
    /// Optional wall-clock timeout for this node.
    pub timeout: Option<std::time::Duration>,
    /// Grace before SIGKILL after timeout/interrupt for this node.
    pub termination_grace: Option<std::time::Duration>,
    /// Context name when this node uses an execution context.
    pub context_name: Option<String>,
    /// Whether this node requires interactive confirmation before spawn.
    pub confirm: bool,
    /// Workspace CAS plan when this node is a cacheable workspace action.
    pub workspace_cache: Option<WorkspaceCachePlan>,
    /// Absolute flake root for workspace CAS paths.
    pub flake_root: Utf8PathBuf,
    /// Highest preparation stage reached for this node.
    pub prep_stage: NodePrepStage,
}

/// Once-per-invocation workspace evaluation: flake, Nix adapter, apps, optional tasks.
///
/// Task runs resolve flake → detect system → evaluate tasks → discover apps once,
/// validate referenced apps, then prepare nodes as they approach execution (or
/// eagerly when lazy prep is disabled).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    pub flake: FlakeSelection,
    pub nix: NixAdapter,
    pub apps: BTreeMap<String, App>,
    pub tasks: Option<TaskDocument>,
    pub invocation_directory: Utf8PathBuf,
    /// Known `devShells.<system>.*` leaf names from flake show.
    pub dev_shells: BTreeSet<String>,
}

/// Per-invocation holder for Nix adapter probes and workspace snapshots.
///
/// Doctor, watch, and task paths thread one instance so adapter capability probes
/// and discovery run at most once per CLI invocation.
#[derive(Debug)]
pub struct WorkspaceState<'a> {
    flake_arg: Option<&'a str>,
    nix_override: Option<&'a str>,
    nix_flags: &'a OptionalNixFlags,
    refresh_discovery: bool,
    adapter: Option<NixAdapter>,
    snapshot_apps: Option<WorkspaceSnapshot>,
    snapshot_tasks: Option<WorkspaceSnapshot>,
}

impl<'a> WorkspaceState<'a> {
    /// Create an empty holder for the given invocation inputs.
    #[must_use]
    pub fn new(
        flake_arg: Option<&'a str>,
        nix_override: Option<&'a str>,
        nix_flags: &'a OptionalNixFlags,
    ) -> Self {
        Self::with_refresh(flake_arg, nix_override, nix_flags, false)
    }

    /// Like [`Self::new`], optionally bypassing the discovery cache.
    #[must_use]
    pub fn with_refresh(
        flake_arg: Option<&'a str>,
        nix_override: Option<&'a str>,
        nix_flags: &'a OptionalNixFlags,
        refresh_discovery: bool,
    ) -> Self {
        Self {
            flake_arg,
            nix_override,
            nix_flags,
            refresh_discovery,
            adapter: None,
            snapshot_apps: None,
            snapshot_tasks: None,
        }
    }

    /// Locate `nix` and run capability probes at most once.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when the executable cannot be located or probed.
    pub fn adapter(&mut self) -> Result<&NixAdapter, NixError> {
        if self.adapter.is_none() {
            self.adapter = Some(build_adapter(self.nix_override)?);
        }
        Ok(self.adapter.as_ref().expect("adapter cached after success"))
    }

    /// Locate `nix` and run capability probes, optionally bypassing the cache.
    ///
    /// # Errors
    ///
    /// Returns [`NixError`] when the executable cannot be located or probed.
    pub fn adapter_refresh(&mut self, refresh: bool) -> Result<&NixAdapter, NixError> {
        if refresh || self.adapter.is_none() {
            self.adapter = Some(build_adapter_refresh(self.nix_override, refresh)?);
        }
        Ok(self.adapter.as_ref().expect("adapter cached after success"))
    }

    /// Load or reuse a workspace snapshot for the current invocation.
    ///
    /// A tasks-inclusive snapshot satisfies apps-only callers.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when directories, flake selection, Nix, or discovery fail.
    pub fn snapshot(&mut self, load_tasks: bool) -> Result<&WorkspaceSnapshot, PrepareError> {
        self.ensure_snapshot(load_tasks)?;
        if load_tasks {
            return Ok(self
                .snapshot_tasks
                .as_ref()
                .expect("tasks snapshot ensured"));
        }
        if let Some(snapshot) = self.snapshot_tasks.as_ref() {
            return Ok(snapshot);
        }
        Ok(self.snapshot_apps.as_ref().expect("apps snapshot ensured"))
    }

    fn ensure_snapshot(&mut self, load_tasks: bool) -> Result<(), PrepareError> {
        if load_tasks {
            if self.snapshot_tasks.is_none() {
                let adapter = self.adapter().map_err(PrepareError::Nix)?.clone();
                self.snapshot_tasks = Some(WorkspaceSnapshot::build(
                    self.flake_arg,
                    true,
                    self.nix_flags,
                    adapter,
                    self.refresh_discovery,
                )?);
            }
            return Ok(());
        }

        if self.snapshot_tasks.is_some() {
            return Ok(());
        }

        if self.snapshot_apps.is_none() {
            let adapter = self.adapter().map_err(PrepareError::Nix)?.clone();
            self.snapshot_apps = Some(WorkspaceSnapshot::build(
                self.flake_arg,
                false,
                self.nix_flags,
                adapter,
                self.refresh_discovery,
            )?);
        }
        Ok(())
    }

    /// Drop cached snapshots so the next [`Self::snapshot`] call rediscovers.
    ///
    /// The Nix adapter (capability probes) is retained.
    pub fn invalidate_snapshots(&mut self) {
        self.snapshot_apps = None;
        self.snapshot_tasks = None;
    }

    /// Discovery cache lookup context for the current invocation inputs.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when directories, flake selection, or Nix probing fail.
    pub fn discovery_context(&mut self) -> Result<DiscoveryContext, PrepareError> {
        let invocation_directory = current_invocation_directory()?;
        let flake = resolve_flake(self.flake_arg, &invocation_directory)?;
        let adapter = self.adapter().map_err(PrepareError::Nix)?;
        Ok(DiscoveryContext {
            flake_ref: flake.nix_ref.clone(),
            local_root: flake.local_root.clone(),
            system: adapter.system.clone(),
            nix_path: adapter.nix.as_str().to_owned(),
            nix_version: adapter.capabilities.version.to_string(),
            discovery_inputs: Vec::new(),
        })
    }
}

/// Errors while preparing an app plan.
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("failed to determine invocation directory: {0}")]
    InvocationDirectory(#[source] io::Error),
    #[error("invocation directory is not valid UTF-8")]
    NonUtf8InvocationDirectory,
    #[error("cannot combine --root and --cwd")]
    RootAndCwdConflict,
    #[error("--root requires a local flake path")]
    RootRequiresLocalFlake,
    #[error("task workingDirectory must stay within the flake root (got {value})")]
    WorkingDirectoryOutsideFlakeRoot { value: String },
    #[error("task {task} references unknown devShell {shell}")]
    UnknownDevShell { task: String, shell: String },
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    NotFound(#[from] AppNotFoundError),
    #[error(transparent)]
    TaskDiscovery(#[from] nxr_nix::TaskDiscoveryError),
    #[error(transparent)]
    TaskSchema(#[from] SchemaError),
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error("failed to build workspace cache plan: {0}")]
    WorkspaceCache(#[source] io::Error),
}

impl PrepareError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::InvocationDirectory(_)
            | Self::NonUtf8InvocationDirectory
            | Self::RootRequiresLocalFlake
            | Self::WorkingDirectoryOutsideFlakeRoot { .. } => exit::DISCOVERY,
            Self::RootAndCwdConflict => exit::USAGE,
            Self::UnknownDevShell { .. } => exit::NOT_FOUND,
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::NotFound(error) => error.exit_code(),
            Self::TaskDiscovery(error) => error.exit_code(),
            Self::TaskSchema(_) | Self::Context(_) | Self::WorkspaceCache(_) => {
                nxr_core::diagnostics::exit::EVALUATION
            }
        }
    }
}

/// Strip a single leading `--` separator from forwarded app arguments.
#[must_use]
pub fn strip_one_separator(args: &[String]) -> Vec<String> {
    match args {
        [first, rest @ ..] if first == "--" => rest.to_vec(),
        other => other.to_vec(),
    }
}

/// Discover apps for the selected flake without resolving a target name.
///
/// # Errors
///
/// Returns [`PrepareError`] when directories, flake selection, or discovery fail.
pub fn discover_apps(request: DiscoverRequest<'_>) -> Result<DiscoveredApps, PrepareError> {
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        false,
        request.nix_flags,
    )?;
    Ok(DiscoveredApps {
        apps: snapshot.apps.into_values().collect(),
    })
}

/// Resolve invocation CWD, flake, apps, and build a [`Plan`].
///
/// Performs app discovery (`nix flake show`) so callers can distinguish missing
/// apps (with suggestions) before execution. Prefer
/// [`prepare_fast_app_plan`] for bare `nxr <app>` / `nxr run` execution.
///
/// # Errors
///
/// Returns [`PrepareError`] when directories, flake selection, discovery, or
/// app resolution fail.
pub fn prepare_app_plan(request: &AppRequest<'_>) -> Result<PreparedPlan, PrepareError> {
    let mut state = WorkspaceState::new(request.flake_arg, request.nix_override, request.nix_flags);
    prepare_app_plan_in_state(request, &mut state)
}

/// Prepare an app plan using a shared per-invocation workspace holder.
///
/// When the prepared-plan disk cache is enabled and fingerprints match, skips
/// discovery and argv assembly. Live env/secret values are still resolved at spawn.
///
/// # Errors
///
/// Same as [`prepare_app_plan`].
pub fn prepare_app_plan_in_state(
    request: &AppRequest<'_>,
    state: &mut WorkspaceState<'_>,
) -> Result<PreparedPlan, PrepareError> {
    let _timer = PlanPrepareGuard::start();
    if plan_cache_enabled()
        && let Some(hit) = try_prepared_plan_cache(request, state, PlanPrepareKind::Discovered)?
    {
        record_plan_cache_hit();
        return Ok(hit);
    }
    let snapshot = state.snapshot(false)?;
    let prepared = snapshot.prepare_discovered_app(request)?;
    if plan_cache_enabled() {
        store_prepared_plan_cache_with_version(
            request,
            &snapshot.flake,
            PlanPrepareKind::Discovered,
            &prepared,
            &snapshot.nix.capabilities.version.to_string(),
        );
        record_plan_cache_miss();
    }
    Ok(prepared)
}

/// Build a [`Plan`] for `nix run <flake>#<app>` without adapter probes.
///
/// Locates `nix` only (no `currentSystem` / capability probes) unless the user
/// requested Required flags (`--offline` / `--accept-flake-config`), which need
/// a one-shot capability check. Missing apps surface as Nix failures; callers
/// may optionally discover afterward for "did you mean?" suggestions when
/// stderr indicates an installable-resolution failure.
///
/// When the prepared-plan disk cache hits, reuses the stored argv without
/// rebuilding it. Live env values are still applied at spawn.
///
/// # Errors
///
/// Returns [`PrepareError`] when directories, flake selection, or Nix location fail.
pub fn prepare_fast_app_plan(request: &AppRequest<'_>) -> Result<PreparedPlan, PrepareError> {
    let _timer = PlanPrepareGuard::start();
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let nix = locate_nix_path(request.nix_override)?;

    if plan_cache_enabled()
        && let Some(hit) = try_fast_prepared_plan_cache(
            request,
            &flake,
            &nix,
            &invocation_cwd,
            &execution_directory,
        )?
    {
        record_plan_cache_hit();
        return Ok(hit);
    }

    // Display-only placeholder: `nix run <flake>#<app>` does not need currentSystem.
    let app = synthetic_app(request.app, &flake.nix_ref, "local");
    let forwarded = strip_one_separator(request.args);
    let plan = build_fast_plan(
        request,
        &flake,
        &nix,
        &app,
        &invocation_cwd,
        &execution_directory,
        &forwarded,
    )?;

    let prepared = PreparedPlan {
        plan,
        nix,
        execution_directory,
        local_root: flake.local_root.clone(),
    };
    if plan_cache_enabled() {
        store_prepared_plan_cache_with_version(
            request,
            &flake,
            PlanPrepareKind::Fast,
            &prepared,
            "",
        );
        record_plan_cache_miss();
    }
    Ok(prepared)
}

/// Locate `nix` without system/capability probes.
///
/// # Errors
///
/// Returns [`NixError::NixNotFound`] when the executable is missing.
pub fn locate_nix_path(nix_override: Option<&str>) -> Result<Utf8PathBuf, NixError> {
    match nix_override {
        Some(path) => {
            let nix = Utf8PathBuf::from(path);
            if !nix.is_file() {
                return Err(NixError::NixNotFound { path: nix });
            }
            Ok(nix)
        }
        None => locate_nix(),
    }
}

fn coalesced_discovery_error(error: nxr_nix::CoalescedDiscoveryError) -> PrepareError {
    match error {
        nxr_nix::CoalescedDiscoveryError::Nix(error) => PrepareError::Nix(error),
        nxr_nix::CoalescedDiscoveryError::Tasks(error) => PrepareError::TaskDiscovery(error),
        nxr_nix::CoalescedDiscoveryError::InvalidEnvelope { source } => {
            PrepareError::Nix(NixError::InvalidJson { source })
        }
        nxr_nix::CoalescedDiscoveryError::ParseApps(error) => PrepareError::Nix(error.into()),
    }
}

pub(crate) struct ColdWorkspaceDiscovery {
    pub(crate) discovery: WorkspaceDiscovery,
}

/// Discover apps and optional tasks.
///
/// Preference order:
/// 1. Optional `nxrMetadata.<system>` (one targeted eval) when enabled
/// 2. Coalesced `{ inventory, nxr }` eval when Determinate (or forced)
/// 3. Classic `flake show` + optional task `eval`
pub(crate) fn cold_discover_workspace(
    nix: &NixAdapter,
    flake_ref: &str,
    load_tasks: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<ColdWorkspaceDiscovery, PrepareError> {
    cold_discover_workspace_with_root(nix, flake_ref, None, load_tasks, nix_flags)
}

pub(crate) fn cold_discover_workspace_with_root(
    nix: &NixAdapter,
    flake_ref: &str,
    local_root: Option<&Utf8Path>,
    load_tasks: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<ColdWorkspaceDiscovery, PrepareError> {
    let worker = local_root
        .and_then(|root| {
            nxr_nix::eval_worker_context_for(
                nix.nix.as_str(),
                &nix.version_banner,
                nix.config_json.as_deref(),
                root,
            )
        })
        .or_else(|| {
            nxr_nix::local_root_from_flake_ref(flake_ref).and_then(|root| {
                nxr_nix::eval_worker_context_for(
                    nix.nix.as_str(),
                    &nix.version_banner,
                    nix.config_json.as_deref(),
                    &root,
                )
            })
        });

    if nxr_nix::nxr_metadata_preferred() {
        let mut discovery_flags = nix_flags.clone();
        discovery_flags.no_write_lock_file = true;
        let args = nix.compatible_argv(
            nxr_nix::nxr_metadata_eval_args(flake_ref, &nix.system),
            &discovery_flags,
        )?;
        match nxr_nix::discover_nxr_metadata_with_worker(
            &nix.nix,
            &nix.system,
            &args,
            worker.as_ref(),
        ) {
            Ok(Some(document)) => match document.into_workspace(flake_ref, &nix.system, load_tasks)
            {
                Ok(workspace) => {
                    return Ok(ColdWorkspaceDiscovery {
                        discovery: WorkspaceDiscovery {
                            apps: workspace.apps,
                            tasks: workspace.tasks,
                            dev_shells: workspace.dev_shells,
                        },
                    });
                }
                Err(error) => {
                    eprintln!("nxr: nxrMetadata parse failed, falling back: {error}");
                }
            },
            Ok(None) => {}
            Err(error) => {
                eprintln!("nxr: nxrMetadata discovery failed, falling back: {error}");
            }
        }
    }

    let eval_plan =
        nxr_nix::plan_discovery_eval(&nix.version_banner, nix.config_json.as_deref(), load_tasks);

    if eval_plan.use_coalesced_discovery {
        let mut discovery_flags = nix_flags.clone();
        discovery_flags.no_write_lock_file = true;
        let args = nix.compatible_argv(
            nxr_nix::coalesced_discovery_args(flake_ref, &nix.system),
            &discovery_flags,
        )?;
        match nxr_nix::discover_coalesced(&nix.nix, &nix.system, flake_ref, &args)
            .map_err(coalesced_discovery_error)
        {
            Ok(coalesced) => {
                let workspace = coalesced
                    .into_workspace(flake_ref, &nix.system, load_tasks)
                    .map_err(coalesced_discovery_error)?;
                return Ok(ColdWorkspaceDiscovery {
                    discovery: WorkspaceDiscovery {
                        apps: workspace.apps,
                        tasks: workspace.tasks,
                        dev_shells: workspace.dev_shells,
                    },
                });
            }
            Err(error) => {
                eprintln!("nxr: coalesced discovery failed, falling back: {error}");
            }
        }
    }

    let show = nix
        .flake_show_json(flake_ref, nix_flags)
        .map_err(PrepareError::Nix)?;
    let dev_shells =
        parse_outputs_from_flake_show(&show, flake_ref, &nix.system, OutputTable::DevShells)
            .map_err(|error| PrepareError::Nix(error.into()))?
            .into_iter()
            .map(|shell| shell.name)
            .collect::<Vec<_>>();
    let apps = parse_apps_from_flake_show(&show, flake_ref, &nix.system)
        .map_err(|error| PrepareError::Nix(error.into()))?;
    let tasks = if load_tasks {
        let mut discovery_flags = nix_flags.clone();
        discovery_flags.no_write_lock_file = true;
        let args = nix.compatible_argv(
            nxr_nix::flake_eval_json_args(flake_ref, &nxr_nix::tasks_attr_path(&nix.system)),
            &discovery_flags,
        )?;
        Some(
            nxr_nix::discover_tasks_with_worker(&nix.nix, &nix.system, &args, worker.as_ref())
                .map_err(PrepareError::TaskDiscovery)?,
        )
    } else if flake_show_has_nxr_for_system(&show, &nix.system) {
        None
    } else {
        Some(TaskDocument::new(BTreeMap::new()))
    };

    Ok(ColdWorkspaceDiscovery {
        discovery: WorkspaceDiscovery {
            apps,
            tasks,
            dev_shells,
        },
    })
}

/// One targeted `nxrMetadata` or `nxr.<system>` eval for app listing metadata.
///
/// Used by the live file-backed fast path when warm discovery cache is cold.
pub(crate) fn cold_discover_app_listings(
    nix: &NixAdapter,
    flake_ref: &str,
    local_root: Option<&Utf8Path>,
    nix_flags: &OptionalNixFlags,
) -> Result<Option<BTreeMap<String, nxr_task::AppListingMetadata>>, PrepareError> {
    let worker = local_root
        .and_then(|root| {
            nxr_nix::eval_worker_context_for(
                nix.nix.as_str(),
                &nix.version_banner,
                nix.config_json.as_deref(),
                root,
            )
        })
        .or_else(|| {
            nxr_nix::local_root_from_flake_ref(flake_ref).and_then(|root| {
                nxr_nix::eval_worker_context_for(
                    nix.nix.as_str(),
                    &nix.version_banner,
                    nix.config_json.as_deref(),
                    &root,
                )
            })
        });

    let mut discovery_flags = nix_flags.clone();
    discovery_flags.no_write_lock_file = true;

    if nxr_nix::nxr_metadata_preferred() {
        let args = nix.compatible_argv(
            nxr_nix::nxr_metadata_eval_args(flake_ref, &nix.system),
            &discovery_flags,
        )?;
        match nxr_nix::discover_nxr_metadata_with_worker(
            &nix.nix,
            &nix.system,
            &args,
            worker.as_ref(),
        ) {
            Ok(Some(document)) => {
                let workspace = document
                    .into_workspace(flake_ref, &nix.system, true)
                    .map_err(metadata_discovery_prepare_error)?;
                if let Some(task_document) = workspace.tasks
                    && !task_document.apps.is_empty()
                {
                    return Ok(Some(task_document.apps));
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("nxr: nxrMetadata listing eval failed, falling back: {error}");
            }
        }
    }

    let args = nix.compatible_argv(
        nxr_nix::flake_eval_json_args(flake_ref, &nxr_nix::tasks_attr_path(&nix.system)),
        &discovery_flags,
    )?;
    match nxr_nix::discover_tasks_with_worker(&nix.nix, &nix.system, &args, worker.as_ref()) {
        Ok(document) => {
            if document.apps.is_empty() {
                Ok(None)
            } else {
                Ok(Some(document.apps))
            }
        }
        Err(error) => Err(PrepareError::TaskDiscovery(error)),
    }
}

fn metadata_discovery_prepare_error(error: nxr_nix::MetadataDiscoveryError) -> PrepareError {
    match error {
        nxr_nix::MetadataDiscoveryError::Nix(error) => PrepareError::Nix(error),
        nxr_nix::MetadataDiscoveryError::Tasks(error) => PrepareError::TaskDiscovery(error),
        error => PrepareError::TaskDiscovery(nxr_nix::TaskDiscoveryError::Schema(
            nxr_task::SchemaError::InvalidDocument {
                message: error.to_string(),
            },
        )),
    }
}

/// Whether stderr from a failed `nix run` indicates a missing installable/app.
#[must_use]
pub fn stderr_indicates_missing_installable(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("does not provide attribute")
        || lower.contains("does not provide")
            && (lower.contains("attribute") || lower.contains("app"))
        || lower.contains("error: attribute '")
        || lower.contains("was not found in the flake")
        || lower.contains("flake has no attribute")
}

impl WorkspaceSnapshot {
    /// Resolve flake, locate Nix / detect system once, discover apps, optionally tasks.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when directories, flake selection, Nix, or discovery fail.
    pub fn load(
        flake_arg: Option<&str>,
        nix_override: Option<&str>,
        load_tasks: bool,
        nix_flags: &OptionalNixFlags,
    ) -> Result<Self, PrepareError> {
        Self::load_with_refresh(flake_arg, nix_override, load_tasks, nix_flags, false)
    }

    /// Like [`Self::load`], optionally bypassing the discovery cache.
    pub fn load_with_refresh(
        flake_arg: Option<&str>,
        nix_override: Option<&str>,
        load_tasks: bool,
        nix_flags: &OptionalNixFlags,
        refresh_discovery: bool,
    ) -> Result<Self, PrepareError> {
        let nix = build_adapter(nix_override)?;
        Self::build(flake_arg, load_tasks, nix_flags, nix, refresh_discovery)
    }

    fn build(
        flake_arg: Option<&str>,
        load_tasks: bool,
        nix_flags: &OptionalNixFlags,
        nix: NixAdapter,
        refresh_discovery: bool,
    ) -> Result<Self, PrepareError> {
        let invocation_directory = current_invocation_directory()?;
        let flake = resolve_flake(flake_arg, &invocation_directory)?;
        let context = DiscoveryContext {
            flake_ref: flake.nix_ref.clone(),
            local_root: flake.local_root.clone(),
            system: nix.system.clone(),
            nix_path: nix.nix.as_str().to_owned(),
            nix_version: nix.capabilities.version.to_string(),
            discovery_inputs: Vec::new(),
        };
        let flake_ref = flake.nix_ref.clone();
        let discovery = if !refresh_discovery
            && let Some(warm) = try_daemon_discovery_get(&context, load_tasks)
        {
            warm
        } else {
            let discovery = discover_workspace_with_cache(
                &context,
                DiscoveryCacheOptions {
                    refresh: refresh_discovery,
                    require_tasks: load_tasks,
                },
                || {
                    let cold = cold_discover_workspace_with_root(
                        &nix,
                        &flake_ref,
                        flake.local_root.as_deref(),
                        load_tasks,
                        nix_flags,
                    )?;
                    Ok::<WorkspaceDiscovery, PrepareError>(cold.discovery)
                },
            )?;
            try_daemon_discovery_put(&context, &discovery);
            discovery
        };
        let dev_shells: BTreeSet<String> = discovery.dev_shells.iter().cloned().collect();
        let apps = discovery
            .apps
            .into_iter()
            .map(|app| (app.name.clone(), app))
            .collect();

        Ok(Self {
            flake,
            nix,
            apps,
            tasks: discovery.tasks,
            invocation_directory,
            dev_shells,
        })
    }

    /// Prepare an app plan using already-discovered apps in this snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when the app is missing or cwd flags conflict.
    pub fn prepare_discovered_app(
        &self,
        request: &AppRequest<'_>,
    ) -> Result<PreparedPlan, PrepareError> {
        let apps: Vec<App> = self.apps.values().cloned().collect();
        let app = resolve_app_by_name(&apps, request.app)?;
        let execution_directory = resolve_execution_directory(
            &self.invocation_directory,
            &self.flake,
            request.root,
            request.cwd,
        )?;
        let forwarded = strip_one_separator(request.args);
        let plan = build_plan(
            request,
            &self.flake,
            &self.nix,
            app,
            &self.invocation_directory,
            &execution_directory,
            &forwarded,
        )?;

        Ok(PreparedPlan {
            plan,
            nix: self.nix.nix.clone(),
            execution_directory,
            local_root: self.flake.local_root.clone(),
        })
    }

    /// Ensure every task's `app` field resolves against discovered apps.
    ///
    /// # Errors
    ///
    /// Returns [`AppNotFoundError`] when a task references an unknown app.
    pub fn validate_task_apps(&self, document: &TaskDocument) -> Result<(), AppNotFoundError> {
        let apps: Vec<App> = self.apps.values().cloned().collect();
        for definition in document.tasks.values() {
            resolve_app_by_name(&apps, definition.app.as_str())?;
        }
        Ok(())
    }

    /// Build spawn plans for every node in `serial_order` without further Nix discovery.
    ///
    /// Eager path used by dry-run, explain, cache explain, watch reuse, and
    /// `NXR_LAZY_PREP=off`. Live task runs prefer [`TaskNodePreparer`].
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when an app is missing or cwd flags conflict.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_task_nodes(
        &self,
        document: &TaskDocument,
        root_task_ids: &[String],
        serial_order: &[String],
        request_args: &[String],
        root: bool,
        cwd: Option<&str>,
        shell: Option<&str>,
        shell_mode: ShellMode,
        environment_policy: &EnvironmentPolicy,
        nix_flags: &OptionalNixFlags,
        context_override: Option<&str>,
    ) -> Result<BTreeMap<String, PreparedTaskNode>, PrepareError> {
        let mut preparer = TaskNodePreparer::new(
            self,
            document,
            root_task_ids,
            request_args,
            root,
            cwd,
            shell,
            shell_mode,
            environment_policy,
            nix_flags,
            context_override,
        )?;
        preparer.prepare_all(serial_order)?;
        let mut prepared = preparer.into_prepared();
        let flake_root = self
            .flake
            .local_root
            .as_deref()
            .unwrap_or(self.invocation_directory.as_path());
        apply_one_shell_dag_optimization(
            &mut prepared,
            &self.flake,
            &self.nix.nix,
            flake_root,
            shell_mode,
            nix_flags,
        )?;
        Ok(prepared)
    }

    /// Whether any planned node requires project trust (confirm or secrets).
    ///
    /// Scans context metadata without building spawn plans so lazy prep can
    /// enforce trust before the first node is prepared.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when a referenced context is unknown.
    pub fn plan_requires_project_trust(
        document: &TaskDocument,
        plan: &ExecutionPlan,
        environment_policy: &EnvironmentPolicy,
        context_override: Option<&str>,
    ) -> Result<bool, PrepareError> {
        for node in &plan.nodes {
            let definition = document
                .tasks
                .get(&node.id)
                .expect("execution plan only includes known task ids");
            let effective_context = context_override.or(definition.context.as_deref());
            if let Some(name) = effective_context {
                let applied = apply_task_context(document, &node.id, name, environment_policy)?;
                if applied.confirm || !applied.plan_secrets.is_empty() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Staged / lazy task-node preparation for the scheduler (ADR-0158 / ADR-0159).
///
/// Phase 1 (DAG / affected) and phase 2 (resource readiness) remain outside this
/// type. Callers prepare only nodes approaching execution (phase 3) and may
/// speculate likely successors within a bounded pool (phase 4).
///
/// Live lazy runs split [`NodePrepStage::CasInputs`] from
/// [`NodePrepStage::SpawnPlan`] so CAS lookup can overlap argv assembly; cache
/// hits cancel or skip SpawnPlan. `NXR_CAS_PLAN_PIPELINE=off` fuses stages.
pub struct TaskNodePreparer<'a> {
    mode: PrepMode<'a>,
    prepared: BTreeMap<String, PreparedTaskNode>,
    prepare_count: u64,
    spawn_plan_count: u64,
    spawn_plan_cancelled: u64,
    /// When false, CasInputs advances through SpawnPlan in one shot (fused).
    pipeline: bool,
    /// Whether ADR-0129 one-shell optimization already ran for this preparer.
    one_shell_applied: bool,
}

enum PrepMode<'a> {
    /// Can prepare additional nodes from the workspace snapshot.
    Live(Box<LivePrep<'a>>),
    /// Watch reuse / pre-built map — lookups only, no further prepare.
    Sealed { document: Option<&'a TaskDocument> },
}

struct LivePrep<'a> {
    snapshot: &'a WorkspaceSnapshot,
    document: &'a TaskDocument,
    root_task_ids: &'a [String],
    request_args: &'a [String],
    root: bool,
    cwd: Option<&'a str>,
    shell: Option<&'a str>,
    shell_mode: ShellMode,
    environment_policy: &'a EnvironmentPolicy,
    nix_flags: &'a OptionalNixFlags,
    context_override: Option<&'a str>,
    upstream_keys: BTreeMap<String, String>,
    digest_cache: RunDigestCache,
    context_hints: BTreeMap<String, PrewarmContext>,
    context_hits: u64,
    context_misses: u64,
}

/// In-flight SpawnPlan work that can be cancelled on CAS hit.
pub struct SpawnPlanTicket {
    task_id: String,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<SpawnPlanParts, PrepareError>>>,
}

struct SpawnPlanParts {
    plan: Plan,
    program: Utf8PathBuf,
    arguments: Vec<String>,
}

impl SpawnPlanTicket {
    /// Request cancellation; the worker may still finish but results are dropped.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Task id this ticket prepares.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl<'a> TaskNodePreparer<'a> {
    /// Create an empty preparer bound to a validated task document.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when the document fails schema validation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: &'a WorkspaceSnapshot,
        document: &'a TaskDocument,
        root_task_ids: &'a [String],
        request_args: &'a [String],
        root: bool,
        cwd: Option<&'a str>,
        shell: Option<&'a str>,
        shell_mode: ShellMode,
        environment_policy: &'a EnvironmentPolicy,
        nix_flags: &'a OptionalNixFlags,
        context_override: Option<&'a str>,
    ) -> Result<Self, PrepareError> {
        document.validate().map_err(PrepareError::TaskSchema)?;
        Ok(Self {
            mode: PrepMode::Live(Box::new(LivePrep {
                snapshot,
                document,
                root_task_ids,
                request_args,
                root,
                cwd,
                shell,
                shell_mode,
                environment_policy,
                nix_flags,
                context_override,
                upstream_keys: BTreeMap::new(),
                digest_cache: RunDigestCache::new(),
                context_hints: BTreeMap::new(),
                context_hits: 0,
                context_misses: 0,
            })),
            prepared: BTreeMap::new(),
            prepare_count: 0,
            spawn_plan_count: 0,
            spawn_plan_cancelled: 0,
            // Pipeline only applies to live lazy runs; sealed/eager fuse stages.
            pipeline: cas_plan_pipeline_enabled(),
            one_shell_applied: false,
        })
    }

    /// Wrap an already-prepared map (watch reuse into the scheduler).
    #[must_use]
    pub fn from_prepared(
        prepared: BTreeMap<String, PreparedTaskNode>,
        document: Option<&'a TaskDocument>,
    ) -> Self {
        let prepare_count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
        let spawn_plan_count = prepare_count;
        let prepared = prepared
            .into_iter()
            .map(|(id, mut node)| {
                node.prep_stage = NodePrepStage::SpawnPlan;
                (id, node)
            })
            .collect();
        Self {
            mode: PrepMode::Sealed { document },
            prepared,
            prepare_count,
            spawn_plan_count,
            spawn_plan_cancelled: 0,
            pipeline: false,
            one_shell_applied: true,
        }
    }

    /// Task definition for a prepared node when document metadata is available.
    #[must_use]
    pub fn task_definition(&self, task_id: &str) -> Option<&nxr_task::TaskDefinition> {
        match &self.mode {
            PrepMode::Live(live) => live.document.tasks.get(task_id),
            PrepMode::Sealed { document } => document.and_then(|doc| doc.tasks.get(task_id)),
        }
    }

    /// Live preparer seeded with existing nodes and a shared digest cache (watch).
    ///
    /// Used after source-only invalidation drops affected prepared nodes while
    /// retaining Merkle / action-digest session state across generations.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when the document fails schema validation.
    #[allow(clippy::too_many_arguments)]
    pub fn from_partial_prepared(
        mut prepared: BTreeMap<String, PreparedTaskNode>,
        snapshot: &'a WorkspaceSnapshot,
        document: &'a TaskDocument,
        root_task_ids: &'a [String],
        request_args: &'a [String],
        root: bool,
        cwd: Option<&'a str>,
        shell: Option<&'a str>,
        shell_mode: ShellMode,
        environment_policy: &'a EnvironmentPolicy,
        nix_flags: &'a OptionalNixFlags,
        context_override: Option<&'a str>,
        digest_cache: RunDigestCache,
        context_hints: BTreeMap<String, PrewarmContext>,
    ) -> Result<Self, PrepareError> {
        document.validate().map_err(PrepareError::TaskSchema)?;
        for node in prepared.values_mut() {
            node.prep_stage = NodePrepStage::SpawnPlan;
        }
        let prepare_count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
        Ok(Self {
            mode: PrepMode::Live(Box::new(LivePrep {
                snapshot,
                document,
                root_task_ids,
                request_args,
                root,
                cwd,
                shell,
                shell_mode,
                environment_policy,
                nix_flags,
                context_override,
                upstream_keys: BTreeMap::new(),
                digest_cache,
                context_hints,
                context_hits: 0,
                context_misses: 0,
            })),
            prepared,
            prepare_count,
            spawn_plan_count: prepare_count,
            spawn_plan_cancelled: 0,
            pipeline: false,
            one_shell_applied: false,
        })
    }

    /// How many nodes reached at least CasInputs.
    #[must_use]
    pub fn prepare_count(&self) -> u64 {
        self.prepare_count
    }

    /// How many SpawnPlan stages completed.
    #[must_use]
    #[allow(dead_code)] // exercised in unit tests; useful for diagnostics
    pub fn spawn_plan_count(&self) -> u64 {
        self.spawn_plan_count
    }

    /// How many SpawnPlan stages were cancelled on CAS hit.
    #[must_use]
    #[allow(dead_code)] // exercised in unit tests; useful for diagnostics
    pub fn spawn_plan_cancelled(&self) -> u64 {
        self.spawn_plan_cancelled
    }

    /// Drain watch prewarm context hit/miss counts from a partial reprepare pass.
    #[must_use]
    pub fn take_prewarm_context_stats(&mut self) -> (u64, u64) {
        let PrepMode::Live(live) = &mut self.mode else {
            return (0, 0);
        };
        let hits = live.context_hits;
        let misses = live.context_misses;
        live.context_hits = 0;
        live.context_misses = 0;
        (hits, misses)
    }

    /// Whether this preparer splits CasInputs from SpawnPlan.
    #[must_use]
    pub fn pipeline_enabled(&self) -> bool {
        self.pipeline && matches!(self.mode, PrepMode::Live(_))
    }

    /// Test-only override for CAS‖plan pipelining.
    #[cfg(test)]
    pub fn set_pipeline_for_test(&mut self, enabled: bool) {
        self.pipeline = enabled;
    }

    /// Borrow prepared nodes (partial under lazy mode).
    #[must_use]
    pub fn prepared(&self) -> &BTreeMap<String, PreparedTaskNode> {
        &self.prepared
    }

    /// Consume into the prepared map.
    #[must_use]
    pub fn into_prepared(self) -> BTreeMap<String, PreparedTaskNode> {
        self.prepared
    }

    /// Consume into the prepared map and the live digest cache (watch partial prep).
    #[must_use]
    pub fn into_prepared_with_digest_cache(
        self,
    ) -> (BTreeMap<String, PreparedTaskNode>, RunDigestCache) {
        match self.mode {
            PrepMode::Live(live) => (self.prepared, live.digest_cache),
            PrepMode::Sealed { .. } => (self.prepared, RunDigestCache::new()),
        }
    }

    /// Eagerly prepare every id in `serial_order` through SpawnPlan.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when any node fails to prepare.
    pub fn prepare_all(&mut self, serial_order: &[String]) -> Result<(), PrepareError> {
        let _timer = PlanPrepareGuard::start();
        for task_id in serial_order {
            self.ensure_prepared(task_id)?;
        }
        Ok(())
    }

    /// Apply ADR-0129 one-shell optimization when every wrapped node shares a shell.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when shell materialization fails.
    pub fn try_apply_one_shell(&mut self, serial_order: &[String]) -> Result<(), PrepareError> {
        if self.one_shell_applied {
            return Ok(());
        }
        let (flake, nix, flake_root, shell_mode, nix_flags) = match &self.mode {
            PrepMode::Live(live) => {
                let flake_root = live
                    .snapshot
                    .flake
                    .local_root
                    .as_deref()
                    .unwrap_or(live.snapshot.invocation_directory.as_path());
                (
                    live.snapshot.flake.clone(),
                    live.snapshot.nix.nix.clone(),
                    flake_root.to_path_buf(),
                    live.shell_mode,
                    live.nix_flags.clone(),
                )
            }
            PrepMode::Sealed { .. } => return Ok(()),
        };
        self.ensure_prepared_many(serial_order)?;
        if apply_one_shell_dag_optimization(
            &mut self.prepared,
            &flake,
            &nix,
            &flake_root,
            shell_mode,
            &nix_flags,
        )? {
            self.one_shell_applied = true;
        }
        Ok(())
    }

    /// Ensure `task_id` is fully prepared (both CAS inputs and spawn plan).
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when preparation fails.
    pub fn ensure_prepared(&mut self, task_id: &str) -> Result<&PreparedTaskNode, PrepareError> {
        self.ensure_stage(task_id, NodePrepStage::SpawnPlan)
    }

    /// Ensure the node has at least `stage` prepared.
    ///
    /// When pipelining is enabled, [`NodePrepStage::CasInputs`] stops after action
    /// keys / digests; [`NodePrepStage::SpawnPlan`] finalizes nix argv via
    /// `build_plan`. When pipelining is off (or sealed), CasInputs advances
    /// through SpawnPlan.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when preparation fails, or when a sealed map
    /// is missing `task_id`.
    pub fn ensure_stage(
        &mut self,
        task_id: &str,
        stage: NodePrepStage,
    ) -> Result<&PreparedTaskNode, PrepareError> {
        let target = if self.pipeline_enabled() {
            stage
        } else {
            NodePrepStage::SpawnPlan
        };
        self.advance_to_stage(task_id, target)
    }

    /// Prepare CasInputs only for each id (phase 3 under pipelining).
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when any node fails to prepare.
    pub fn ensure_cas_inputs_many(&mut self, ids: &[String]) -> Result<(), PrepareError> {
        for id in ids {
            self.ensure_stage(id, NodePrepStage::CasInputs)?;
        }
        Ok(())
    }

    /// Prepare each id through SpawnPlan (phase 3 when fused / eager).
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when any node fails to prepare.
    pub fn ensure_prepared_many(&mut self, ids: &[String]) -> Result<(), PrepareError> {
        for id in ids {
            self.ensure_prepared(id)?;
        }
        Ok(())
    }

    /// Start SpawnPlan on a background thread after CasInputs (pipeline mode).
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when CasInputs is missing or the node is sealed.
    pub fn start_spawn_plan(&mut self, task_id: &str) -> Result<SpawnPlanTicket, PrepareError> {
        self.ensure_stage(task_id, NodePrepStage::CasInputs)?;
        if self
            .prepared
            .get(task_id)
            .is_some_and(|n| n.prep_stage >= NodePrepStage::SpawnPlan)
        {
            let cancel = Arc::new(AtomicBool::new(false));
            return Ok(SpawnPlanTicket {
                task_id: task_id.to_owned(),
                cancel,
                handle: None,
            });
        }
        let PrepMode::Live(live) = &self.mode else {
            return Err(PrepareError::WorkspaceCache(io::Error::other(
                "sealed preparer cannot start spawn plan",
            )));
        };
        let node = self
            .prepared
            .get(task_id)
            .expect("CasInputs just ensured")
            .clone();
        let root_task_ids = live.root_task_ids.to_vec();
        let request_args = live.request_args.to_vec();
        let root = live.root;
        let cwd = live.cwd.map(str::to_owned);
        let shell = live.shell.map(str::to_owned);
        let shell_mode = live.shell_mode;
        let environment_policy = live.environment_policy.clone();
        let nix_flags = live.nix_flags.clone();
        let context_override = live.context_override.map(str::to_owned);
        let task_id_owned = task_id.to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let flake = live.snapshot.flake.clone();
        let nix = live.snapshot.nix.clone();
        let apps = live.snapshot.apps.clone();
        let invocation_directory = live.snapshot.invocation_directory.clone();
        let document = live.document.clone();
        let handle = std::thread::spawn(move || {
            if cancel_worker.load(Ordering::Relaxed) {
                return Err(PrepareError::WorkspaceCache(io::Error::other(
                    "spawn plan cancelled",
                )));
            }
            #[cfg(test)]
            maybe_spawn_plan_test_delay(&cancel_worker);
            if cancel_worker.load(Ordering::Relaxed) {
                return Err(PrepareError::WorkspaceCache(io::Error::other(
                    "spawn plan cancelled",
                )));
            }
            compute_spawn_plan_parts(
                &document,
                &task_id_owned,
                &root_task_ids,
                &request_args,
                root,
                cwd.as_deref(),
                shell.as_deref(),
                shell_mode,
                &environment_policy,
                &nix_flags,
                context_override.as_deref(),
                &flake,
                &nix,
                &apps,
                &invocation_directory,
                &node,
            )
        });
        Ok(SpawnPlanTicket {
            task_id: task_id.to_owned(),
            cancel,
            handle: Some(handle),
        })
    }

    /// Cancel an in-flight SpawnPlan ticket (CAS hit path).
    pub fn cancel_spawn_plan(&mut self, ticket: SpawnPlanTicket) {
        ticket.cancel();
        if let Some(handle) = ticket.handle {
            let _ = handle.join();
        }
        self.spawn_plan_cancelled = self.spawn_plan_cancelled.saturating_add(1);
        record_spawn_plan_cancelled();
    }

    /// Join a SpawnPlan ticket and apply results unless cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when the worker failed (and was not cancelled).
    pub fn join_spawn_plan(&mut self, mut ticket: SpawnPlanTicket) -> Result<(), PrepareError> {
        let Some(handle) = ticket.handle.take() else {
            return Ok(());
        };
        if ticket.cancel.load(Ordering::Relaxed) {
            let _ = handle.join();
            self.spawn_plan_cancelled = self.spawn_plan_cancelled.saturating_add(1);
            record_spawn_plan_cancelled();
            return Ok(());
        }
        match handle.join() {
            Ok(Ok(parts)) => {
                if ticket.cancel.load(Ordering::Relaxed) {
                    self.spawn_plan_cancelled = self.spawn_plan_cancelled.saturating_add(1);
                    record_spawn_plan_cancelled();
                    return Ok(());
                }
                self.apply_spawn_plan_parts(&ticket.task_id, parts)?;
                Ok(())
            }
            Ok(Err(error)) => {
                if ticket.cancel.load(Ordering::Relaxed)
                    || error.to_string().contains("spawn plan cancelled")
                {
                    self.spawn_plan_cancelled = self.spawn_plan_cancelled.saturating_add(1);
                    record_spawn_plan_cancelled();
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Err(_) => Err(PrepareError::WorkspaceCache(io::Error::other(
                "spawn plan worker panicked",
            ))),
        }
    }

    /// Speculatively prepare likely successors of `started` up to `budget` nodes.
    ///
    /// Under pipelining, speculation prepares CasInputs only so never-run nodes
    /// avoid SpawnPlan. Fail-fast callers simply never request speculation.
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] when a speculative prepare fails.
    pub fn speculate_successors(
        &mut self,
        plan: &ExecutionPlan,
        started: &[String],
        budget: usize,
    ) -> Result<(), PrepareError> {
        if budget == 0 || matches!(self.mode, PrepMode::Sealed { .. }) {
            return Ok(());
        }
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in &plan.nodes {
            for dep in &node.depends_on {
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(node.id.as_str());
            }
        }
        let mut remaining = budget;
        for id in started {
            let Some(children) = dependents.get(id.as_str()) else {
                continue;
            };
            for child in children {
                if remaining == 0 {
                    return Ok(());
                }
                if self.prepared.contains_key(*child) {
                    continue;
                }
                if self.pipeline_enabled() {
                    self.ensure_stage(child, NodePrepStage::CasInputs)?;
                } else {
                    self.ensure_prepared(child)?;
                }
                remaining -= 1;
            }
        }
        Ok(())
    }

    /// Flake root used for workspace CAS paths (available before any prepare).
    #[must_use]
    pub fn flake_root(&self) -> Utf8PathBuf {
        if let Some(node) = self.prepared.values().next() {
            return node.flake_root.clone();
        }
        match &self.mode {
            PrepMode::Live(live) => live
                .snapshot
                .flake
                .local_root
                .as_deref()
                .unwrap_or(live.snapshot.invocation_directory.as_path())
                .to_path_buf(),
            PrepMode::Sealed { .. } => Utf8PathBuf::from("."),
        }
    }

    fn advance_to_stage(
        &mut self,
        task_id: &str,
        target: NodePrepStage,
    ) -> Result<&PreparedTaskNode, PrepareError> {
        if let Some(existing) = self.prepared.get(task_id)
            && existing.prep_stage >= target
        {
            return Ok(&self.prepared[task_id]);
        }
        if matches!(self.mode, PrepMode::Sealed { .. }) {
            return Err(PrepareError::WorkspaceCache(io::Error::other(format!(
                "sealed preparer missing node {task_id}"
            ))));
        }
        let _timer = PlanPrepareGuard::start();
        if !self.prepared.contains_key(task_id) {
            let node = self.prepare_cas_inputs(task_id)?;
            self.prepared.insert(task_id.to_owned(), node);
            self.prepare_count = self.prepare_count.saturating_add(1);
            record_node_prepared();
        }
        if target >= NodePrepStage::SpawnPlan
            && self.prepared[task_id].prep_stage < NodePrepStage::SpawnPlan
        {
            self.prepare_spawn_plan_exists(task_id)?;
        }
        Ok(&self.prepared[task_id])
    }

    fn apply_spawn_plan_parts(
        &mut self,
        task_id: &str,
        parts: SpawnPlanParts,
    ) -> Result<(), PrepareError> {
        let Some(node) = self.prepared.get_mut(task_id) else {
            return Err(PrepareError::WorkspaceCache(io::Error::other(format!(
                "missing node {task_id} when applying spawn plan"
            ))));
        };
        if node.prep_stage >= NodePrepStage::SpawnPlan {
            return Ok(());
        }
        node.plan = parts.plan;
        node.program = parts.program;
        node.arguments = parts.arguments;
        node.prep_stage = NodePrepStage::SpawnPlan;
        self.spawn_plan_count = self.spawn_plan_count.saturating_add(1);
        record_spawn_plan_prepared();
        Ok(())
    }

    fn prepare_spawn_plan_exists(&mut self, task_id: &str) -> Result<(), PrepareError> {
        let node = self
            .prepared
            .get(task_id)
            .expect("caller ensures CasInputs")
            .clone();
        let PrepMode::Live(live) = &self.mode else {
            return Err(PrepareError::WorkspaceCache(io::Error::other(
                "sealed preparer cannot prepare spawn plan",
            )));
        };
        let parts = compute_spawn_plan_parts(
            live.document,
            task_id,
            live.root_task_ids,
            live.request_args,
            live.root,
            live.cwd,
            live.shell,
            live.shell_mode,
            live.environment_policy,
            live.nix_flags,
            live.context_override,
            &live.snapshot.flake,
            &live.snapshot.nix,
            &live.snapshot.apps,
            &live.snapshot.invocation_directory,
            &node,
        )?;
        self.apply_spawn_plan_parts(task_id, parts)
    }

    fn prepare_cas_inputs(&mut self, task_id: &str) -> Result<PreparedTaskNode, PrepareError> {
        let PrepMode::Live(live) = &mut self.mode else {
            return Err(PrepareError::WorkspaceCache(io::Error::other(
                "sealed preparer cannot prepare nodes",
            )));
        };
        let LivePrep {
            snapshot,
            document,
            root_task_ids,
            request_args,
            root,
            cwd,
            shell,
            shell_mode,
            environment_policy,
            nix_flags,
            context_override,
            upstream_keys,
            digest_cache,
            context_hints,
            context_hits,
            context_misses,
        } = live.as_mut();

        let definition = document
            .tasks
            .get(task_id)
            .expect("execution plan only includes known task ids");
        let apps: Vec<App> = snapshot.apps.values().cloned().collect();
        let forwarded = if root_task_ids.iter().any(|id| id == task_id) {
            *request_args
        } else {
            &[][..]
        };
        let app = resolve_app_by_name(&apps, definition.app.as_str())?;
        let execution_directory = resolve_task_execution_directory(
            &snapshot.invocation_directory,
            &snapshot.flake,
            *root,
            *cwd,
            definition.working_directory.as_deref(),
        )?;
        let mut context_name = None;
        let mut confirm = false;
        let effective_context = context_override.or(definition.context.as_deref());
        let (applied_context, node_environment, effective_shell) =
            if let Some(cached) = context_hints.get(task_id) {
                *context_hits = context_hits.saturating_add(1);
                (
                    cached.applied_context.clone(),
                    cached.environment_policy.clone(),
                    cached.effective_shell.clone(),
                )
            } else {
                *context_misses = context_misses.saturating_add(1);
                let applied_context = effective_context
                    .map(|name| apply_task_context(document, task_id, name, environment_policy))
                    .transpose()?;
                let context_shell = applied_context
                    .as_ref()
                    .and_then(|applied| applied.shell.clone());
                let node_environment = if let Some(applied) = &applied_context {
                    applied.environment_policy.clone()
                } else {
                    (*environment_policy).clone()
                };
                let effective_shell =
                    resolve_effective_shell(*shell, context_shell, definition.shell.clone());
                (applied_context, node_environment, effective_shell)
            };
        if let Some(cached) = context_hints.get(task_id) {
            context_name = cached.context_name.clone();
            confirm = cached.confirm;
        } else if let Some(applied) = &applied_context {
            context_name = Some(applied.context_name.clone());
            confirm = applied.confirm;
        }
        if let Some(shell_name) = effective_shell_wrap(effective_shell.as_deref(), *shell_mode)
            && !snapshot.dev_shells.contains(shell_name)
        {
            return Err(PrepareError::UnknownDevShell {
                task: task_id.to_owned(),
                shell: shell_name.to_owned(),
            });
        }

        // CasInputs needs stable command argv for the action key without committing
        // SpawnPlan; assemble the same argv build_plan uses.
        let command_argv = assemble_nix_run_argv(
            &snapshot.flake,
            &snapshot.nix,
            app,
            effective_shell.as_deref(),
            *shell_mode,
            nix_flags,
            &strip_one_separator(forwarded),
        )?;

        let timeout = definition
            .timeout
            .as_deref()
            .map(nxr_task::parse_duration)
            .transpose()
            .map_err(|error| {
                PrepareError::TaskSchema(SchemaError::InvalidTimeout {
                    task: task_id.to_owned(),
                    message: error.to_string(),
                })
            })?;
        let termination_grace = definition
            .termination_grace_period
            .as_deref()
            .map(nxr_task::parse_duration)
            .transpose()
            .map_err(|error| {
                PrepareError::TaskSchema(SchemaError::InvalidTimeout {
                    task: task_id.to_owned(),
                    message: error.to_string(),
                })
            })?;
        let flake_root = snapshot
            .flake
            .local_root
            .as_deref()
            .unwrap_or(snapshot.invocation_directory.as_path());
        let workspace_cache = build_workspace_cache_plan(
            document,
            task_id,
            definition,
            &snapshot.nix.system,
            flake_root,
            execution_directory.as_str(),
            upstream_keys,
            &WorkspaceCachePlanOptions {
                forwarded_args: forwarded.to_vec(),
                command_program: Some(snapshot.nix.nix.to_string()),
                command_argv: command_argv.clone(),
                effective_shell: effective_shell.clone(),
                environment_policy: Some(node_environment.clone()),
                context_name: context_name.clone(),
                context_secrets: applied_context
                    .as_ref()
                    .map(|applied| applied.plan_secrets.clone())
                    .unwrap_or_default(),
                context_spawn_env_set: applied_context
                    .as_ref()
                    .map(|applied| applied.spawn_env_set.clone())
                    .unwrap_or_default(),
            },
            Some(digest_cache),
        )
        .map_err(PrepareError::WorkspaceCache)?;
        if let Some(key) = workspace_cache.action_key.as_ref() {
            upstream_keys.insert(task_id.to_owned(), key.clone());
        }

        let mut plan = Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: snapshot.flake.nix_ref.clone(),
            system: snapshot.nix.system.clone(),
            target: app.name.clone(),
            attr_path: app.attr_path.clone(),
            invocation_directory: snapshot.invocation_directory.as_str().to_owned(),
            execution_directory: execution_directory.as_str().to_owned(),
            shell: effective_shell.clone(),
            active_shell: active_dev_shell(),
            environment_policy: (*environment_policy).clone(),
            context: None,
            secrets: Vec::new(),
            context_env_set: BTreeMap::new(),
            parameters: parameter_names(&definition.parameters),
            command: PlanCommand {
                program: snapshot.nix.nix.as_str().to_owned(),
                arguments: command_argv.clone(),
            },
            forwarded_arguments: forwarded.to_vec(),
            workspace_script: None,
            mutable_source: false,
            fallback_app: None,
            environment_mode: None,
        };
        if let Some(applied) = applied_context.as_ref() {
            plan.context = Some(applied.context_name.clone());
            plan.secrets = plan_secrets_for_core(&applied.plan_secrets);
            plan.context_env_set = applied.spawn_env_set.clone();
            plan.environment_policy = applied.environment_policy.clone();
        }

        Ok(PreparedTaskNode {
            id: task_id.to_owned(),
            program: snapshot.nix.nix.clone(),
            arguments: command_argv,
            cwd: execution_directory,
            environment: node_environment,
            plan,
            timeout,
            termination_grace,
            context_name,
            confirm,
            workspace_cache: Some(workspace_cache),
            flake_root: flake_root.to_path_buf(),
            prep_stage: NodePrepStage::CasInputs,
        })
    }
}

/// Absolute UTF-8 path of the process working directory.
///
/// # Errors
///
/// Returns [`PrepareError`] when the current directory cannot be read or is not UTF-8.
pub fn current_invocation_directory() -> Result<Utf8PathBuf, PrepareError> {
    let cwd = std::env::current_dir().map_err(PrepareError::InvocationDirectory)?;
    Utf8PathBuf::from_path_buf(cwd).map_err(|_| PrepareError::NonUtf8InvocationDirectory)
}

/// Build a [`NixAdapter`], optionally overriding the `nix` executable.
///
/// # Errors
///
/// Returns [`NixError`] when the executable cannot be located or the system cannot be detected.
pub fn build_adapter(nix_override: Option<&str>) -> Result<NixAdapter, NixError> {
    build_adapter_refresh(nix_override, false)
}

/// Build a [`NixAdapter`], optionally bypassing the capability cache.
///
/// # Errors
///
/// Returns [`NixError`] when the executable cannot be located or the system cannot be detected.
pub fn build_adapter_refresh(
    nix_override: Option<&str>,
    refresh: bool,
) -> Result<NixAdapter, NixError> {
    match nix_override {
        Some(path) => {
            let nix = Utf8PathBuf::from(path);
            if !nix.is_file() {
                return Err(NixError::NixNotFound { path: nix });
            }
            if refresh {
                NixAdapter::from_nix_refresh(nix)
            } else {
                NixAdapter::from_nix(nix)
            }
        }
        None if refresh => {
            let nix = nxr_nix::locate_nix()?;
            NixAdapter::from_nix_refresh(nix)
        }
        None => NixAdapter::new(),
    }
}

/// Synthesize an [`App`] for the bare-app fast path (no discovery metadata).
#[must_use]
pub fn synthetic_app(name: &str, flake_ref: &str, system: &str) -> App {
    App {
        name: name.to_owned(),
        attr_path: format!("apps.{system}.{name}"),
        flake_ref: flake_ref.to_owned(),
        system: system.to_owned(),
        description: None,
        is_default: name == "default",
        metadata: BTreeMap::new(),
    }
}

/// After a failed fast-path `nix run`, discover apps and map missing names to suggestions.
///
/// Returns `Ok(None)` when the app exists (caller should keep the original exit code)
/// or discovery fails. Returns `Ok(Some(error))` when the app is absent.
///
/// # Errors
///
/// Only returns [`PrepareError`] for directory / flake / adapter failures during
/// the optional discovery pass (not for missing apps).
pub fn suggest_missing_app_after_run(
    request: &AppRequest<'_>,
) -> Result<Option<AppNotFoundError>, PrepareError> {
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        false,
        request.nix_flags,
    )?;
    let apps: Vec<App> = snapshot.apps.values().cloned().collect();
    match resolve_app_by_name(&apps, request.app) {
        Ok(_) => Ok(None),
        Err(error) => Ok(Some(error)),
    }
}

/// Resolve the child working directory from CLI `--root` / `--cwd`.
///
/// # Errors
///
/// Returns [`PrepareError`] when flags conflict or paths cannot be resolved.
pub fn resolve_execution_directory(
    invocation_cwd: &Utf8Path,
    flake: &FlakeSelection,
    root: bool,
    cwd: Option<&str>,
) -> Result<Utf8PathBuf, PrepareError> {
    match (root, cwd) {
        (true, Some(_)) => Err(PrepareError::RootAndCwdConflict),
        (true, None) => flake
            .local_root
            .clone()
            .ok_or(PrepareError::RootRequiresLocalFlake),
        (false, Some(path)) => {
            let joined = if Path::new(path).is_absolute() {
                Utf8PathBuf::from(path)
            } else {
                invocation_cwd.join(path)
            };
            Ok(joined.canonicalize_utf8().unwrap_or(joined))
        }
        (false, None) => Ok(invocation_cwd.to_path_buf()),
    }
}

/// Resolve per-task execution directory with CLI precedence.
///
/// Precedence: CLI `--root` / `--cwd` > task `workingDirectory` > invocation directory.
///
/// # Errors
///
/// Returns [`PrepareError`] when CLI flags conflict, `flake-root` requires a
/// local flake, or task metadata is invalid.
pub fn resolve_task_execution_directory(
    invocation_cwd: &Utf8Path,
    flake: &FlakeSelection,
    root: bool,
    cwd: Option<&str>,
    task_working_directory: Option<&str>,
) -> Result<Utf8PathBuf, PrepareError> {
    if root || cwd.is_some() {
        return resolve_execution_directory(invocation_cwd, flake, root, cwd);
    }

    let Some(token) = task_working_directory else {
        return Ok(invocation_cwd.to_path_buf());
    };

    match token {
        WORKING_DIRECTORY_INVOCATION => Ok(invocation_cwd.to_path_buf()),
        WORKING_DIRECTORY_FLAKE_ROOT => flake
            .local_root
            .clone()
            .ok_or(PrepareError::RootRequiresLocalFlake),
        relative => {
            let flake_root = flake
                .local_root
                .as_ref()
                .ok_or(PrepareError::RootRequiresLocalFlake)?;
            resolve_flake_relative_working_directory(flake_root, relative)
        }
    }
}

fn resolve_flake_relative_working_directory(
    flake_root: &Utf8Path,
    relative: &str,
) -> Result<Utf8PathBuf, PrepareError> {
    let joined = flake_root.join(relative);
    let canonical_flake_root = flake_root
        .canonicalize_utf8()
        .unwrap_or_else(|_| flake_root.to_path_buf());
    let canonical = joined.canonicalize_utf8().unwrap_or(joined);
    if !canonical.starts_with(&canonical_flake_root) {
        return Err(PrepareError::WorkingDirectoryOutsideFlakeRoot {
            value: relative.to_owned(),
        });
    }
    Ok(canonical)
}

/// Assemble nix-run argv used in action keys (same inputs as [`build_plan`]).
fn assemble_nix_run_argv(
    flake: &FlakeSelection,
    adapter: &NixAdapter,
    app: &App,
    shell: Option<&str>,
    shell_mode: ShellMode,
    nix_flags: &OptionalNixFlags,
    forwarded: &[String],
) -> Result<Vec<String>, NixError> {
    let run_argv = nix_run_args(&flake.nix_ref, &app.name, forwarded);
    let wrap_shell = effective_shell_wrap(shell, shell_mode);
    let base_arguments = match wrap_shell {
        Some(shell_name) => {
            nix_develop_wrap_run_args(adapter.nix.as_str(), &flake.nix_ref, shell_name, &run_argv)
        }
        None => run_argv,
    };
    adapter.compatible_argv(base_arguments, nix_flags)
}

fn analyze_one_shell_dag_eligibility(
    nodes: &[&PreparedTaskNode],
    shell_mode: ShellMode,
) -> Option<String> {
    if matches!(shell_mode, ShellMode::Never) {
        return None;
    }

    let mut wrapped: Vec<&PreparedTaskNode> = Vec::new();
    for node in nodes {
        let shell = node.plan.shell.as_deref()?;
        if effective_shell_wrap(Some(shell), shell_mode).is_none() {
            continue;
        }
        if strip_nix_develop_wrap(&node.arguments).is_none() {
            continue;
        }
        if node.confirm || !node.plan.secrets.is_empty() {
            return None;
        }
        if node
            .workspace_cache
            .as_ref()
            .is_some_and(|plan| plan.cache_enabled)
        {
            return None;
        }
        wrapped.push(node);
    }

    if wrapped.len() < 2 {
        return None;
    }

    let first = wrapped[0];
    let shell = first.plan.shell.clone()?;
    let environment = &first.environment;
    let environment_policy = &first.plan.environment_policy;
    let context_env_set = &first.plan.context_env_set;

    for node in wrapped.iter().skip(1) {
        if node.plan.shell.as_deref() != Some(shell.as_str()) {
            return None;
        }
        if node.environment != *environment
            || node.plan.environment_policy != *environment_policy
            || node.plan.context_env_set != *context_env_set
        {
            return None;
        }
    }

    Some(shell)
}

/// Strip per-node `nix develop` wraps after materializing a shared shell once.
///
/// # Errors
///
/// Returns [`PrepareError`] when Nix shell materialization fails.
pub(crate) fn apply_one_shell_dag_optimization(
    prepared: &mut BTreeMap<String, PreparedTaskNode>,
    flake: &FlakeSelection,
    nix: &Utf8Path,
    flake_root: &Utf8Path,
    shell_mode: ShellMode,
    nix_flags: &OptionalNixFlags,
) -> Result<bool, PrepareError> {
    let refs: Vec<&PreparedTaskNode> = prepared.values().collect();
    let Some(shell) = analyze_one_shell_dag_eligibility(&refs, shell_mode) else {
        return Ok(false);
    };

    let sample = refs
        .iter()
        .find(|node| {
            node.plan.shell.as_deref() == Some(shell.as_str())
                && strip_nix_develop_wrap(&node.arguments).is_some()
        })
        .expect("eligibility ensures a wrapped node exists");
    let base_policy = sample.environment.clone();

    let (materialized, env_mode) = match shell_mode {
        ShellMode::Smart => {
            if let Some(policy) = materialize_process_shell_policy(
                flake,
                nix,
                flake_root,
                &shell,
                &base_policy,
                nix_flags,
            )? {
                (policy, ENV_MODE_PROCESS)
            } else {
                let policy =
                    materialize_develop_shell_policy(flake, nix, &shell, &base_policy, nix_flags)?;
                (policy, ENV_MODE_SHELL)
            }
        }
        ShellMode::Always => {
            let policy =
                materialize_develop_shell_policy(flake, nix, &shell, &base_policy, nix_flags)?;
            (policy, ENV_MODE_SHELL)
        }
        ShellMode::Never => return Ok(false),
    };

    for node in prepared.values_mut() {
        if node.plan.shell.as_deref() != Some(shell.as_str()) {
            continue;
        }
        let Some(mut inner) = strip_nix_develop_wrap(&node.arguments) else {
            continue;
        };
        if inner.first().map(String::as_str) == Some(nix.as_str()) {
            inner.remove(0);
        }
        node.arguments.clone_from(&inner);
        node.plan.command.arguments = inner;
        node.environment = materialized.clone();
        node.plan.environment_policy = materialized.clone();
        node.plan.environment_mode = Some(env_mode.to_owned());
        node.plan.active_shell = active_dev_shell();
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn compute_spawn_plan_parts(
    document: &TaskDocument,
    task_id: &str,
    root_task_ids: &[String],
    request_args: &[String],
    root: bool,
    cwd: Option<&str>,
    shell: Option<&str>,
    shell_mode: ShellMode,
    environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
    context_override: Option<&str>,
    flake: &FlakeSelection,
    nix: &NixAdapter,
    apps: &BTreeMap<String, App>,
    invocation_directory: &Utf8Path,
    cas_node: &PreparedTaskNode,
) -> Result<SpawnPlanParts, PrepareError> {
    let definition = document
        .tasks
        .get(task_id)
        .expect("execution plan only includes known task ids");
    let app_list: Vec<App> = apps.values().cloned().collect();
    let forwarded = if root_task_ids.iter().any(|id| id == task_id) {
        request_args
    } else {
        &[][..]
    };
    let app = resolve_app_by_name(&app_list, definition.app.as_str())?;
    let effective_context = context_override.or(definition.context.as_deref());
    let applied_context = effective_context
        .map(|name| apply_task_context(document, task_id, name, environment_policy))
        .transpose()?;
    let context_shell = applied_context
        .as_ref()
        .and_then(|applied| applied.shell.clone());
    let effective_shell = resolve_effective_shell(shell, context_shell, definition.shell.clone());
    let app_request = AppRequest {
        flake_arg: None,
        nix_override: None,
        app: definition.app.as_str(),
        args: forwarded,
        root,
        cwd,
        shell: effective_shell.as_deref(),
        shell_mode,
        environment_policy: environment_policy.clone(),
        nix_flags,
    };
    let mut plan = build_plan(
        &app_request,
        flake,
        nix,
        app,
        invocation_directory,
        &cas_node.cwd,
        &strip_one_separator(forwarded),
    )?;
    if let Some(applied) = applied_context.as_ref() {
        plan.context = Some(applied.context_name.clone());
        plan.secrets = plan_secrets_for_core(&applied.plan_secrets);
        plan.context_env_set = applied.spawn_env_set.clone();
        plan.environment_policy = applied.environment_policy.clone();
    }
    plan.parameters = parameter_names(&definition.parameters);
    let program = Utf8PathBuf::from(plan.command.program.as_str());
    let arguments = plan.command.arguments.clone();
    Ok(SpawnPlanParts {
        plan,
        program,
        arguments,
    })
}

#[cfg(test)]
fn maybe_spawn_plan_test_delay(cancel: &AtomicBool) {
    let delay_ms = SPAWN_PLAN_TEST_DELAY_MS.load(Ordering::Relaxed);
    if delay_ms == 0 {
        return;
    }
    let steps = delay_ms / 10;
    for _ in 0..steps.max(1) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
static SPAWN_PLAN_TEST_DELAY_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn build_plan(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    adapter: &NixAdapter,
    app: &App,
    invocation_directory: &Utf8Path,
    execution_directory: &Utf8Path,
    forwarded: &[String],
) -> Result<Plan, NixError> {
    let run_argv = nix_run_args(&flake.nix_ref, &app.name, forwarded);
    let wrap_shell = effective_shell_wrap(request.shell, request.shell_mode);
    let base_arguments = match wrap_shell {
        Some(shell_name) => {
            nix_develop_wrap_run_args(adapter.nix.as_str(), &flake.nix_ref, shell_name, &run_argv)
        }
        None => run_argv,
    };
    let command_arguments = adapter.compatible_argv(base_arguments, request.nix_flags)?;

    Ok(Plan {
        schema_version: Plan::SCHEMA_VERSION,
        kind: PlanKind::App,
        flake: flake.nix_ref.clone(),
        system: adapter.system.clone(),
        target: app.name.clone(),
        attr_path: app.attr_path.clone(),
        invocation_directory: invocation_directory.as_str().to_owned(),
        execution_directory: execution_directory.as_str().to_owned(),
        shell: request.shell.map(str::to_owned),
        active_shell: active_dev_shell(),
        environment_policy: request.environment_policy.clone(),
        context: None,
        secrets: Vec::new(),
        context_env_set: BTreeMap::new(),
        parameters: Vec::new(),
        command: PlanCommand {
            program: adapter.nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
        workspace_script: None,
        mutable_source: false,
        fallback_app: None,
        environment_mode: None,
    })
}

fn build_fast_plan(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    nix: &Utf8Path,
    app: &App,
    invocation_directory: &Utf8Path,
    execution_directory: &Utf8Path,
    forwarded: &[String],
) -> Result<Plan, NixError> {
    let run_argv = nix_run_args(&flake.nix_ref, &app.name, forwarded);
    let wrap_shell = effective_shell_wrap(request.shell, request.shell_mode);
    let base_arguments = match wrap_shell {
        Some(shell_name) => {
            nix_develop_wrap_run_args(nix.as_str(), &flake.nix_ref, shell_name, &run_argv)
        }
        None => run_argv,
    };

    let needs_capability_probe = request.nix_flags.offline || request.nix_flags.accept_flake_config;
    let capabilities = if needs_capability_probe {
        detect_capabilities(nix)?
    } else {
        // No RequiredByUser flags: skip probes. Assume floor best-effort support.
        NixCapabilities {
            version: TESTED_NIX_SUPPORT_FLOOR,
            flakes_enabled: true,
            supports_json_log_format: true,
            supports_no_write_lock_file: true,
            supports_offline: false,
            supports_accept_flake_config: false,
            supports_print_dev_env_json: true,
        }
    };
    let command_arguments = capabilities.apply_optional_flags(base_arguments, request.nix_flags)?;

    Ok(Plan {
        schema_version: Plan::SCHEMA_VERSION,
        kind: PlanKind::App,
        flake: flake.nix_ref.clone(),
        system: app.system.clone(),
        target: app.name.clone(),
        attr_path: app.attr_path.clone(),
        invocation_directory: invocation_directory.as_str().to_owned(),
        execution_directory: execution_directory.as_str().to_owned(),
        shell: request.shell.map(str::to_owned),
        active_shell: active_dev_shell(),
        environment_policy: request.environment_policy.clone(),
        context: None,
        secrets: Vec::new(),
        context_env_set: BTreeMap::new(),
        parameters: Vec::new(),
        command: PlanCommand {
            program: nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
        workspace_script: None,
        mutable_source: false,
        fallback_app: None,
        environment_mode: None,
    })
}

fn plan_secrets_for_core(entries: &[PlanSecretEntry]) -> Vec<PlanSecretRef> {
    entries
        .iter()
        .map(|entry| PlanSecretRef {
            name: entry.name.clone(),
            reference: entry.reference.clone(),
            delivery: secret_delivery_label(entry.delivery).to_owned(),
            provider: secret_provider_label(entry.provider).to_owned(),
            value: "<runtime>".to_owned(),
        })
        .collect()
}

fn secret_provider_label(provider: nxr_task::SecretProvider) -> &'static str {
    match provider {
        nxr_task::SecretProvider::Env => "env",
        nxr_task::SecretProvider::File => "file",
        nxr_task::SecretProvider::Sops => "sops",
        nxr_task::SecretProvider::SopsNix => "sops-nix",
    }
}

fn secret_delivery_label(delivery: SecretDelivery) -> &'static str {
    match delivery {
        SecretDelivery::Env => "env",
        SecretDelivery::File => "file",
        SecretDelivery::Stdin => "stdin",
    }
}

fn try_prepared_plan_cache(
    request: &AppRequest<'_>,
    state: &mut WorkspaceState<'_>,
    prepare_kind: PlanPrepareKind,
) -> Result<Option<PreparedPlan>, PrepareError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let Some(local_root) = flake.local_root.as_ref() else {
        return Ok(None);
    };
    let adapter = state.adapter().map_err(PrepareError::Nix)?;
    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let app = synthetic_app(request.app, &flake.nix_ref, &adapter.system);
    let fingerprints = shared_fingerprints(
        local_root,
        adapter.nix.as_str(),
        &adapter.capabilities.version.to_string(),
    )?;
    let Some(fingerprints) = fingerprints else {
        return Ok(None);
    };
    let key = build_plan_cache_key(
        request,
        prepare_kind,
        &flake,
        local_root.as_str(),
        &adapter.system,
        &app.attr_path,
        invocation_cwd.as_str(),
        execution_directory.as_str(),
        fingerprints.clone(),
    );
    let key_digest = plan_cache_key_digest(&key);
    if let Some(hit) = try_daemon_plan_lookup(&key_digest, &fingerprints, prepare_kind) {
        return Ok(Some(PreparedPlan {
            plan: hit.plan,
            nix: Utf8PathBuf::from(hit.nix),
            execution_directory: Utf8PathBuf::from(hit.execution_directory),
            local_root: flake.local_root.clone(),
        }));
    }
    let Some(hit) = lookup_prepared_plan(&key_digest, &fingerprints) else {
        return Ok(None);
    };
    if hit.prepare_kind != prepare_kind {
        return Ok(None);
    }
    try_daemon_plan_store(
        &key_digest,
        prepare_kind,
        &hit.plan,
        &hit.nix,
        &hit.execution_directory,
        fingerprints,
    );
    Ok(Some(PreparedPlan {
        plan: hit.plan,
        nix: Utf8PathBuf::from(hit.nix),
        execution_directory: Utf8PathBuf::from(hit.execution_directory),
        local_root: flake.local_root.clone(),
    }))
}

fn try_fast_prepared_plan_cache(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    nix: &Utf8Path,
    invocation_cwd: &Utf8Path,
    execution_directory: &Utf8Path,
) -> Result<Option<PreparedPlan>, PrepareError> {
    let Some(local_root) = flake.local_root.as_ref() else {
        return Ok(None);
    };
    let app = synthetic_app(request.app, &flake.nix_ref, "local");
    let fingerprints = shared_fingerprints(local_root, nix.as_str(), "")?;
    let Some(fingerprints) = fingerprints else {
        return Ok(None);
    };
    let key = build_plan_cache_key(
        request,
        PlanPrepareKind::Fast,
        flake,
        local_root.as_str(),
        "local",
        &app.attr_path,
        invocation_cwd.as_str(),
        execution_directory.as_str(),
        fingerprints.clone(),
    );
    let key_digest = plan_cache_key_digest(&key);
    if let Some(hit) = try_daemon_plan_lookup(&key_digest, &fingerprints, PlanPrepareKind::Fast) {
        return Ok(Some(PreparedPlan {
            plan: hit.plan,
            nix: Utf8PathBuf::from(hit.nix),
            execution_directory: Utf8PathBuf::from(hit.execution_directory),
            local_root: flake.local_root.clone(),
        }));
    }
    let Some(hit) = lookup_prepared_plan(&key_digest, &fingerprints) else {
        return Ok(None);
    };
    if hit.prepare_kind != PlanPrepareKind::Fast {
        return Ok(None);
    }
    try_daemon_plan_store(
        &key_digest,
        PlanPrepareKind::Fast,
        &hit.plan,
        &hit.nix,
        &hit.execution_directory,
        fingerprints,
    );
    Ok(Some(PreparedPlan {
        plan: hit.plan,
        nix: Utf8PathBuf::from(hit.nix),
        execution_directory: Utf8PathBuf::from(hit.execution_directory),
        local_root: flake.local_root.clone(),
    }))
}

fn store_prepared_plan_cache_with_version(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    prepare_kind: PlanPrepareKind,
    prepared: &PreparedPlan,
    nix_version: &str,
) {
    let Some(local_root) = flake.local_root.as_ref() else {
        return;
    };
    let Ok(Some(fingerprints)) =
        shared_fingerprints(local_root, prepared.nix.as_str(), nix_version)
    else {
        return;
    };
    let system = match prepare_kind {
        PlanPrepareKind::Fast => "local",
        PlanPrepareKind::Discovered => prepared.plan.system.as_str(),
    };
    // Keep attr_path key material aligned with the lookup path (synthetic apps.<system>.<name>).
    let attr_path = format!("apps.{system}.{}", request.app);
    let key = build_plan_cache_key(
        request,
        prepare_kind,
        flake,
        local_root.as_str(),
        system,
        &attr_path,
        prepared.plan.invocation_directory.as_str(),
        prepared.execution_directory.as_str(),
        fingerprints.clone(),
    );
    let key_digest = plan_cache_key_digest(&key);
    let _ = store_prepared_plan(
        &key_digest,
        prepare_kind,
        &prepared.plan,
        prepared.nix.as_str(),
        prepared.execution_directory.as_str(),
        fingerprints.clone(),
    );
    try_daemon_plan_store(
        &key_digest,
        prepare_kind,
        &prepared.plan,
        prepared.nix.as_str(),
        prepared.execution_directory.as_str(),
        fingerprints,
    );
}

fn try_daemon_plan_lookup(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
    prepare_kind: PlanPrepareKind,
) -> Option<nxr_core::PreparedPlanCacheHit> {
    let result: serde_json::Value = try_once(
        "plan.get",
        Some(serde_json::json!({ "key_digest": key_digest })),
    )?;
    if result.get("hit").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let entry: nxr_core::DaemonPlanEntry =
        serde_json::from_value(result.get("entry")?.clone()).ok()?;
    if entry.prepare_kind != prepare_kind || &entry.fingerprints != expected {
        return None;
    }
    let hit = daemon_plan_to_hit(entry);
    record_plan_cache_hit();
    Some(hit)
}

fn try_daemon_plan_store(
    key_digest: &str,
    prepare_kind: PlanPrepareKind,
    plan: &Plan,
    nix: &str,
    execution_directory: &str,
    fingerprints: PlanCacheSharedFingerprints,
) {
    let Some(entry) = daemon_plan_entry(prepare_kind, plan, nix, execution_directory, fingerprints)
    else {
        return;
    };
    let _: Option<serde_json::Value> = try_once(
        "plan.put",
        Some(serde_json::json!({
            "key_digest": key_digest,
            "entry": entry,
        })),
    );
}

fn discovery_daemon_key(context: &DiscoveryContext, require_tasks: bool) -> String {
    format!(
        "{}|{}|{}|{}|{}|tasks={}",
        context.flake_ref,
        context.local_root.as_ref().map_or("", |p| p.as_str()),
        context.system,
        context.nix_path,
        context.nix_version,
        require_tasks
    )
}

fn try_daemon_discovery_get(
    context: &DiscoveryContext,
    require_tasks: bool,
) -> Option<WorkspaceDiscovery> {
    let key = discovery_daemon_key(context, require_tasks);
    let result: serde_json::Value =
        try_once("discovery.get", Some(serde_json::json!({ "key": key })))?;
    if result.get("hit").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let payload = result.get("payload")?;
    let apps: Vec<App> = serde_json::from_value(payload.get("apps")?.clone()).ok()?;
    let tasks = match payload.get("tasks") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    if require_tasks && tasks.is_none() {
        return None;
    }
    let dev_shells: Vec<String> = payload
        .get("dev_shells")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Some(WorkspaceDiscovery {
        apps,
        tasks,
        dev_shells,
    })
}

fn try_daemon_discovery_put(context: &DiscoveryContext, discovery: &WorkspaceDiscovery) {
    let key = discovery_daemon_key(context, discovery.tasks.is_some());
    let payload = serde_json::json!({
        "apps": discovery.apps,
        "tasks": discovery.tasks,
        "dev_shells": discovery.dev_shells,
    });
    let _: Option<serde_json::Value> = try_once(
        "discovery.put",
        Some(serde_json::json!({
            "key": key,
            "payload": payload,
        })),
    );
}

#[allow(clippy::too_many_arguments)]
fn build_plan_cache_key(
    request: &AppRequest<'_>,
    prepare_kind: PlanPrepareKind,
    flake: &FlakeSelection,
    local_root: &str,
    system: &str,
    attr_path: &str,
    invocation_directory: &str,
    execution_directory: &str,
    fingerprints: PlanCacheSharedFingerprints,
) -> PlanCacheKeyMaterial {
    PlanCacheKeyMaterial {
        prepare_kind,
        flake_ref: flake.nix_ref.clone(),
        local_root: local_root.to_owned(),
        system: system.to_owned(),
        app_name: request.app.to_owned(),
        attr_path: attr_path.to_owned(),
        nix_flags_digest: digest_nix_flags(
            request.nix_flags.offline,
            request.nix_flags.no_write_lock_file,
            request.nix_flags.accept_flake_config,
            request.nix_flags.json_log_format,
            &request.nix_flags.nix_options,
            &request.nix_flags.extra_argv,
        ),
        shell_name: request.shell.map(str::to_owned),
        shell_mode: shell_mode_label(request.shell_mode).to_owned(),
        active_shell: active_dev_shell(),
        root: request.root,
        cwd: request.cwd.map(str::to_owned),
        invocation_directory: invocation_directory.to_owned(),
        execution_directory: execution_directory.to_owned(),
        environment_policy_digest: digest_environment_policy(&request.environment_policy),
        forwarded_arguments: strip_one_separator(request.args),
        fingerprints,
    }
}

fn shell_mode_label(mode: ShellMode) -> &'static str {
    match mode {
        ShellMode::Smart => "smart",
        ShellMode::Always => "always",
        ShellMode::Never => "never",
    }
}

fn shared_fingerprints(
    local_root: &Utf8Path,
    nix_path: &str,
    nix_version: &str,
) -> Result<Option<PlanCacheSharedFingerprints>, PrepareError> {
    let nix_tree = match nix_tree_fingerprint(local_root) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let discovery_inputs = hint_discovery_inputs_for_root(local_root);
    let discovery_inputs_fp = match discovery_inputs_fingerprint(local_root, &discovery_inputs) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let source_identity = git_source_identity(local_root)
        .ok()
        .flatten()
        .map(|identity| identity.digest);
    let flake_lock = flake_lock_digest(local_root).ok().flatten();
    let nix_file_identity = nix_executable_identity(nix_path);
    Ok(Some(PlanCacheSharedFingerprints {
        nix_tree_fingerprint: nix_tree,
        discovery_inputs_fingerprint: discovery_inputs_fp,
        flake_lock_digest: flake_lock,
        nix_path: nix_path.to_owned(),
        nix_version: nix_version.to_owned(),
        nix_file_identity,
        source_identity,
    }))
}

fn nix_executable_identity(nix_path: &str) -> Option<String> {
    let path = Utf8Path::new(nix_path);
    let canonical = path
        .canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf());
    let metadata = fs::metadata(canonical.as_std_path()).ok()?;
    let modified = metadata.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let modified_secs = since_epoch.as_secs();
    let modified_nanos = since_epoch.subsec_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "{}:{}:{}:{}:{}",
            metadata.len(),
            modified_secs,
            modified_nanos,
            metadata.dev(),
            metadata.ino()
        ))
    }
    #[cfg(not(unix))]
    {
        Some(format!(
            "{}:{}:{}",
            metadata.len(),
            modified_secs,
            modified_nanos
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;

    use super::{
        AppRequest, PrepareError, build_plan, resolve_execution_directory,
        resolve_task_execution_directory, strip_one_separator, synthetic_app,
    };
    use crate::flake::FlakeSelection;
    use crate::shell_mode::ShellMode;
    use nxr_core::App;
    use nxr_nix::NixAdapter;
    use nxr_nix::OptionalNixFlags;
    use nxr_task::{WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION};

    #[test]
    fn strip_one_separator_removes_only_leading_double_dash() {
        assert_eq!(
            strip_one_separator(&["--".to_owned(), "--nocapture".to_owned()]),
            vec!["--nocapture".to_owned()]
        );
        assert_eq!(
            strip_one_separator(&["--".to_owned(), "--".to_owned(), "extra".to_owned()]),
            vec!["--".to_owned(), "extra".to_owned()]
        );
        assert_eq!(
            strip_one_separator(&["--nocapture".to_owned()]),
            vec!["--nocapture".to_owned()]
        );
        assert_eq!(strip_one_separator(&[]), Vec::<String>::new());
    }

    #[test]
    fn root_and_cwd_conflict_is_usage_error() {
        let flake = FlakeSelection {
            display: ".".to_owned(),
            nix_ref: "/tmp/project".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/tmp/project")),
        };
        let error = resolve_execution_directory(
            camino::Utf8Path::new("/tmp/project/crates"),
            &flake,
            true,
            Some("elsewhere"),
        )
        .expect_err("conflict");
        assert!(matches!(error, PrepareError::RootAndCwdConflict));
        assert_eq!(error.exit_code(), nxr_core::diagnostics::exit::USAGE);
    }

    #[test]
    fn synthetic_app_builds_attr_path_without_discovery() {
        let app = synthetic_app("hello", "/abs/fixtures/basic-apps", "aarch64-darwin");
        assert_eq!(app.name, "hello");
        assert_eq!(app.attr_path, "apps.aarch64-darwin.hello");
        assert!(!app.is_default);
        assert!(app.metadata.is_empty());
    }

    #[test]
    fn build_plan_uses_nix_run_args_and_strips_nothing_twice() {
        let flake = FlakeSelection {
            display: "fixtures/basic-apps".to_owned(),
            nix_ref: "/abs/fixtures/basic-apps".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/abs/fixtures/basic-apps")),
        };
        let adapter = NixAdapter::with_nix_and_system(
            camino::Utf8PathBuf::from("/nix/bin/nix"),
            "aarch64-darwin".to_owned(),
        );
        let app = App {
            name: "hello".to_owned(),
            attr_path: "apps.aarch64-darwin.hello".to_owned(),
            flake_ref: flake.nix_ref.clone(),
            system: "aarch64-darwin".to_owned(),
            description: None,
            is_default: false,
            metadata: BTreeMap::new(),
        };
        let forwarded = strip_one_separator(&["--".to_owned(), "one".to_owned()]);
        let nix_flags = OptionalNixFlags::default();
        let request = AppRequest {
            flake_arg: None,
            nix_override: None,
            app: "hello",
            args: &["--".to_owned(), "one".to_owned()],
            root: false,
            cwd: None,
            shell: None,
            shell_mode: ShellMode::Smart,
            environment_policy: nxr_core::EnvironmentPolicy::Inherit,
            nix_flags: &nix_flags,
        };
        let plan = build_plan(
            &request,
            &flake,
            &adapter,
            &app,
            camino::Utf8Path::new("/work"),
            camino::Utf8Path::new("/work"),
            &forwarded,
        )
        .expect("build plan");

        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.target, "hello");
        assert_eq!(plan.command.program, "/nix/bin/nix");
        assert_eq!(
            plan.command.arguments,
            vec![
                "run".to_owned(),
                "/abs/fixtures/basic-apps#hello".to_owned(),
                "--".to_owned(),
                "one".to_owned(),
            ]
        );
        assert_eq!(plan.forwarded_arguments, vec!["one".to_owned()]);
    }

    #[test]
    fn resolve_task_execution_directory_honors_cli_over_task_metadata() {
        let flake = FlakeSelection {
            display: "fixtures/nested-directory".to_owned(),
            nix_ref: "/tmp/project".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/tmp/project")),
        };
        let invocation = camino::Utf8Path::new("/tmp/project/deep/down/here");

        let from_task = resolve_task_execution_directory(
            invocation,
            &flake,
            false,
            None,
            Some(WORKING_DIRECTORY_FLAKE_ROOT),
        )
        .expect("task flake-root");
        assert_eq!(from_task, camino::Utf8PathBuf::from("/tmp/project"));

        let from_cli = resolve_task_execution_directory(
            invocation,
            &flake,
            false,
            Some("override"),
            Some(WORKING_DIRECTORY_FLAKE_ROOT),
        )
        .expect("cli cwd wins");
        assert_eq!(
            from_cli,
            camino::Utf8PathBuf::from("/tmp/project/deep/down/here/override")
        );

        let from_root = resolve_task_execution_directory(
            invocation,
            &flake,
            true,
            None,
            Some(WORKING_DIRECTORY_INVOCATION),
        )
        .expect("cli root wins");
        assert_eq!(from_root, camino::Utf8PathBuf::from("/tmp/project"));
    }

    #[test]
    fn resolve_task_execution_directory_rejects_parent_traversal() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let flake_root =
            camino::Utf8PathBuf::from_path_buf(temp.path().join("flake")).expect("utf8 temp path");
        std::fs::create_dir_all(flake_root.join("crates")).expect("crates dir");
        std::fs::create_dir(temp.path().join("outside")).expect("outside dir");

        let flake = FlakeSelection {
            display: flake_root.as_str().to_owned(),
            nix_ref: format!("path:{flake_root}"),
            local_root: Some(flake_root.clone()),
        };
        let invocation = flake_root.join("crates");

        let err =
            resolve_task_execution_directory(&invocation, &flake, false, None, Some("../outside"))
                .expect_err("parent traversal escapes flake root");
        assert!(matches!(
            err,
            PrepareError::WorkingDirectoryOutsideFlakeRoot { .. }
        ));
    }

    #[test]
    fn resolve_task_execution_directory_matrix() {
        let flake = FlakeSelection {
            display: "fixtures/nested-directory".to_owned(),
            nix_ref: "/tmp/project".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/tmp/project")),
        };
        let invocation = camino::Utf8PathBuf::from("/tmp/project/deep/down/here");

        assert_eq!(
            resolve_task_execution_directory(
                &invocation,
                &flake,
                false,
                None,
                Some(WORKING_DIRECTORY_INVOCATION),
            )
            .expect("invocation"),
            invocation
        );
        assert_eq!(
            resolve_task_execution_directory(
                &invocation,
                &flake,
                false,
                None,
                Some(WORKING_DIRECTORY_FLAKE_ROOT),
            )
            .expect("flake-root"),
            camino::Utf8PathBuf::from("/tmp/project")
        );
        assert_eq!(
            resolve_task_execution_directory(
                &invocation,
                &flake,
                false,
                None,
                Some("deep/down/here"),
            )
            .expect("relative"),
            invocation
        );
        assert_eq!(
            resolve_task_execution_directory(&invocation, &flake, false, None, None)
                .expect("default"),
            invocation
        );
    }

    #[test]
    fn build_plan_with_shell_wraps_nix_run_in_develop() {
        let flake = FlakeSelection {
            display: "fixtures/named-dev-shells".to_owned(),
            nix_ref: "/abs/fixtures/named-dev-shells".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/abs/fixtures/named-dev-shells")),
        };
        let adapter = NixAdapter::with_nix_and_system(
            camino::Utf8PathBuf::from("/nix/bin/nix"),
            "aarch64-darwin".to_owned(),
        );
        let app = App {
            name: "shell-marker".to_owned(),
            attr_path: "apps.aarch64-darwin.shell-marker".to_owned(),
            flake_ref: flake.nix_ref.clone(),
            system: "aarch64-darwin".to_owned(),
            description: None,
            is_default: false,
            metadata: BTreeMap::new(),
        };
        let nix_flags = OptionalNixFlags::default();
        let request = AppRequest {
            flake_arg: None,
            nix_override: None,
            app: "shell-marker",
            args: &[],
            root: false,
            cwd: None,
            shell: Some("default"),
            shell_mode: ShellMode::Always,
            environment_policy: nxr_core::EnvironmentPolicy::Inherit,
            nix_flags: &nix_flags,
        };
        let plan = build_plan(
            &request,
            &flake,
            &adapter,
            &app,
            camino::Utf8Path::new("/work"),
            camino::Utf8Path::new("/work"),
            &[],
        )
        .expect("build plan");

        assert_eq!(plan.shell.as_deref(), Some("default"));
        assert_eq!(
            plan.command.arguments,
            vec![
                "develop".to_owned(),
                "/abs/fixtures/named-dev-shells#default".to_owned(),
                "-c".to_owned(),
                "/nix/bin/nix".to_owned(),
                "run".to_owned(),
                "/abs/fixtures/named-dev-shells#shell-marker".to_owned(),
            ]
        );
    }

    #[test]
    fn lazy_prep_kill_switch_parses_off_values() {
        assert!(super::lazy_prep_enabled_for_env(None));
        assert!(super::lazy_prep_enabled_for_env(Some("1")));
        assert!(super::lazy_prep_enabled_for_env(Some("on")));
        assert!(!super::lazy_prep_enabled_for_env(Some("off")));
        assert!(!super::lazy_prep_enabled_for_env(Some("0")));
        assert!(!super::lazy_prep_enabled_for_env(Some("false")));
        assert!(!super::lazy_prep_enabled_for_env(Some("NO")));
    }

    #[test]
    fn cas_plan_pipeline_kill_switch_parses_off_values() {
        assert!(super::cas_plan_pipeline_enabled_for_env(None));
        assert!(super::cas_plan_pipeline_enabled_for_env(Some("1")));
        assert!(!super::cas_plan_pipeline_enabled_for_env(Some("off")));
        assert!(!super::cas_plan_pipeline_enabled_for_env(Some("0")));
        assert!(!super::cas_plan_pipeline_enabled_for_env(Some("false")));
        assert!(!super::cas_plan_pipeline_enabled_for_env(Some("no")));
    }

    fn chain_fixture() -> (
        super::WorkspaceSnapshot,
        nxr_task::TaskDocument,
        Vec<String>,
        OptionalNixFlags,
    ) {
        use nxr_task::{FailurePolicy, TaskDefinition, build_execution_plan};

        let mut tasks = BTreeMap::new();
        let mut a = TaskDefinition::new("leaf");
        a.depends_on = Vec::new();
        let mut b = TaskDefinition::new("leaf");
        b.depends_on = vec!["a".to_owned()];
        let mut c = TaskDefinition::new("leaf");
        c.depends_on = vec!["b".to_owned()];
        tasks.insert("a".to_owned(), a);
        tasks.insert("b".to_owned(), b);
        tasks.insert("c".to_owned(), c);
        let document = nxr_task::TaskDocument::new(tasks);
        let plan = build_execution_plan(&document.tasks, "c", FailurePolicy::FailFast, None)
            .expect("plan");

        let flake = FlakeSelection {
            display: "/tmp/lazy-prep".to_owned(),
            nix_ref: "/tmp/lazy-prep".to_owned(),
            local_root: Some(camino::Utf8PathBuf::from("/tmp/lazy-prep")),
        };
        let adapter = NixAdapter::with_nix_and_system(
            camino::Utf8PathBuf::from("/nix/bin/nix"),
            "aarch64-darwin".to_owned(),
        );
        let app = App {
            name: "leaf".to_owned(),
            attr_path: "apps.aarch64-darwin.leaf".to_owned(),
            flake_ref: flake.nix_ref.clone(),
            system: "aarch64-darwin".to_owned(),
            description: None,
            is_default: false,
            metadata: BTreeMap::new(),
        };
        let mut apps = BTreeMap::new();
        apps.insert("leaf".to_owned(), app);
        let snapshot = super::WorkspaceSnapshot {
            flake,
            nix: adapter,
            apps,
            tasks: Some(document.clone()),
            invocation_directory: camino::Utf8PathBuf::from("/tmp/lazy-prep"),
            dev_shells: std::collections::BTreeSet::new(),
        };
        (
            snapshot,
            document,
            plan.serial_order,
            OptionalNixFlags::default(),
        )
    }

    fn pipeline_preparer<'a>(
        snapshot: &'a super::WorkspaceSnapshot,
        document: &'a nxr_task::TaskDocument,
        roots: &'a [String],
        nix_flags: &'a OptionalNixFlags,
        policy: &'a nxr_core::EnvironmentPolicy,
        pipeline: bool,
    ) -> super::TaskNodePreparer<'a> {
        let mut preparer = super::TaskNodePreparer::new(
            snapshot,
            document,
            roots,
            &[],
            false,
            None,
            None,
            ShellMode::Smart,
            policy,
            nix_flags,
            None,
        )
        .expect("preparer");
        preparer.set_pipeline_for_test(pipeline);
        preparer
    }

    #[test]
    fn lazy_preparer_skips_never_run_successors() {
        let (snapshot, document, serial_order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);

        // Simulate fail-fast after the first ready node: only prepare "a".
        preparer.ensure_prepared("a").expect("prepare a");
        assert_eq!(preparer.prepare_count(), 1);
        assert!(preparer.prepared().contains_key("a"));
        assert!(!preparer.prepared().contains_key("b"));
        assert!(!preparer.prepared().contains_key("c"));

        // Eager path still prepares the full serial order.
        let all = snapshot
            .prepare_task_nodes(
                &document,
                &roots,
                &serial_order,
                &[],
                false,
                None,
                None,
                ShellMode::Smart,
                &policy,
                &nix_flags,
                None,
            )
            .expect("eager");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn lazy_preparer_respects_affected_serial_subset() {
        let (snapshot, document, _full_order, nix_flags) = chain_fixture();
        // Affected selection / excluded branches shrink serial_order before prepare.
        let affected_order = vec!["a".to_owned(), "b".to_owned()];
        let roots = vec!["b".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        preparer
            .prepare_all(&affected_order)
            .expect("prepare affected");
        assert_eq!(preparer.prepare_count(), 2);
        assert!(!preparer.prepared().contains_key("c"));
    }

    #[test]
    fn speculate_successors_is_bounded() {
        use nxr_task::{FailurePolicy, build_execution_plan};

        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let plan = build_execution_plan(&document.tasks, "c", FailurePolicy::KeepGoing, None)
            .expect("plan");
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        preparer.ensure_prepared("a").expect("a");
        preparer
            .speculate_successors(&plan, &["a".to_owned()], 1)
            .expect("speculate");
        assert_eq!(preparer.prepare_count(), 2);
        assert!(preparer.prepared().contains_key("b"));
        assert!(!preparer.prepared().contains_key("c"));
        // Speculation under pipelining stops at CasInputs for successors.
        assert_eq!(
            preparer.prepared()["b"].prep_stage,
            super::NodePrepStage::CasInputs
        );
        assert_eq!(preparer.spawn_plan_count(), 1);
    }

    #[test]
    fn cas_plan_pipeline_kill_switch_fuses_stages() {
        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, false);
        preparer
            .ensure_stage("a", super::NodePrepStage::CasInputs)
            .expect("fused cas");
        assert_eq!(
            preparer.prepared()["a"].prep_stage,
            super::NodePrepStage::SpawnPlan
        );
        assert_eq!(preparer.spawn_plan_count(), 1);
        assert_eq!(preparer.spawn_plan_cancelled(), 0);
    }

    #[test]
    fn cas_plan_pipeline_splits_cas_from_spawn() {
        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        preparer
            .ensure_stage("a", super::NodePrepStage::CasInputs)
            .expect("cas");
        assert_eq!(
            preparer.prepared()["a"].prep_stage,
            super::NodePrepStage::CasInputs
        );
        assert_eq!(preparer.spawn_plan_count(), 0);
        preparer
            .ensure_stage("a", super::NodePrepStage::SpawnPlan)
            .expect("spawn");
        assert_eq!(
            preparer.prepared()["a"].prep_stage,
            super::NodePrepStage::SpawnPlan
        );
        assert_eq!(preparer.spawn_plan_count(), 1);
    }

    #[test]
    fn cas_plan_pipeline_hit_cancels_in_flight_spawn_plan() {
        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        preparer
            .ensure_stage("a", super::NodePrepStage::CasInputs)
            .expect("cas");
        super::SPAWN_PLAN_TEST_DELAY_MS.store(200, Ordering::Relaxed);
        let ticket = preparer.start_spawn_plan("a").expect("start");
        // Simulate CAS hit winning the race.
        preparer.cancel_spawn_plan(ticket);
        super::SPAWN_PLAN_TEST_DELAY_MS.store(0, Ordering::Relaxed);
        assert_eq!(preparer.spawn_plan_count(), 0);
        assert_eq!(preparer.spawn_plan_cancelled(), 1);
        assert_eq!(
            preparer.prepared()["a"].prep_stage,
            super::NodePrepStage::CasInputs
        );
    }

    #[test]
    fn cas_plan_pipeline_mixed_hit_prepares_fewer_spawn_plans() {
        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        preparer
            .ensure_cas_inputs_many(&["a".to_owned(), "b".to_owned()])
            .expect("cas both");
        super::SPAWN_PLAN_TEST_DELAY_MS.store(80, Ordering::Relaxed);
        let hit_ticket = preparer.start_spawn_plan("a").expect("start a");
        let miss_ticket = preparer.start_spawn_plan("b").expect("start b");
        // Cache hit on `a` cancels SpawnPlan; miss on `b` keeps it.
        preparer.cancel_spawn_plan(hit_ticket);
        preparer.join_spawn_plan(miss_ticket).expect("join b");
        super::SPAWN_PLAN_TEST_DELAY_MS.store(0, Ordering::Relaxed);
        assert_eq!(preparer.prepare_count(), 2);
        assert_eq!(preparer.spawn_plan_count(), 1);
        assert_eq!(preparer.spawn_plan_cancelled(), 1);
        assert_eq!(
            preparer.prepared()["a"].prep_stage,
            super::NodePrepStage::CasInputs
        );
        assert_eq!(
            preparer.prepared()["b"].prep_stage,
            super::NodePrepStage::SpawnPlan
        );
    }

    #[test]
    fn cas_plan_pipeline_fail_fast_skips_never_run_spawn_plans() {
        let (snapshot, document, _order, nix_flags) = chain_fixture();
        let roots = vec!["c".to_owned()];
        let policy = nxr_core::EnvironmentPolicy::Inherit;
        let mut preparer =
            pipeline_preparer(&snapshot, &document, &roots, &nix_flags, &policy, true);
        // Fail-fast after first node: only CasInputs+SpawnPlan for `a`.
        preparer.ensure_prepared("a").expect("a");
        assert_eq!(preparer.spawn_plan_count(), 1);
        assert!(!preparer.prepared().contains_key("b"));
        assert!(!preparer.prepared().contains_key("c"));
        // Cancel an in-flight speculative ticket as fail-fast would.
        preparer
            .ensure_stage("b", super::NodePrepStage::CasInputs)
            .expect("cas b");
        super::SPAWN_PLAN_TEST_DELAY_MS.store(150, Ordering::Relaxed);
        let ticket = preparer.start_spawn_plan("b").expect("start b");
        preparer.cancel_spawn_plan(ticket);
        super::SPAWN_PLAN_TEST_DELAY_MS.store(0, Ordering::Relaxed);
        assert_eq!(preparer.spawn_plan_count(), 1);
        assert_eq!(preparer.spawn_plan_cancelled(), 1);
        assert_eq!(
            preparer.prepared()["b"].prep_stage,
            super::NodePrepStage::CasInputs
        );
    }
}
