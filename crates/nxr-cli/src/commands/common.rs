//! Shared helpers for list / run / plan commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_completion::{discovery_inputs_fingerprint, nix_tree_fingerprint};
use nxr_core::PlanPrepareGuard;
use nxr_core::diagnostics::exit;
use nxr_core::{
    App, EnvironmentPolicy, Plan, PlanCacheKeyMaterial, PlanCacheSharedFingerprints, PlanCommand,
    PlanKind, PlanPrepareKind, PlanSecretRef, digest_environment_policy, digest_nix_flags,
    flake_lock_digest, lookup_prepared_plan, plan_cache_enabled, plan_cache_key_digest,
    record_plan_cache_hit, record_plan_cache_miss, store_prepared_plan,
};
use nxr_nix::{
    AppNotFoundError, NixAdapter, NixCapabilities, NixError, OptionalNixFlags, OutputTable,
    TESTED_NIX_SUPPORT_FLOOR, detect_capabilities, flake_show_has_nxr_for_system, locate_nix,
    nix_develop_wrap_run_args, nix_run_args, parse_apps_from_flake_show,
    parse_outputs_from_flake_show, resolve_app_by_name,
};
use nxr_task::{
    ContextError, PlanSecretEntry, SchemaError, SecretDelivery, TaskDocument,
    WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION, WorkspaceCachePlan,
    WorkspaceCachePlanOptions, apply_task_context, build_workspace_cache_plan,
};

use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::shell_mode::{
    ShellMode, active_dev_shell, effective_shell_wrap, resolve_effective_shell,
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

/// Precomputed spawn inputs for one task graph node.
///
/// Built once from a [`WorkspaceSnapshot`] before the scheduler starts so node
/// execution does not re-run discovery or system detection.
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
}

/// Once-per-invocation workspace evaluation: flake, Nix adapter, apps, optional tasks.
///
/// Task runs resolve flake → detect system → evaluate tasks → discover apps once,
/// validate referenced apps, then prepare every node before the scheduler starts.
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

/// Discover apps and optional tasks, preferring coalesced eval when available.
pub(crate) fn cold_discover_workspace(
    nix: &NixAdapter,
    flake_ref: &str,
    load_tasks: bool,
    nix_flags: &OptionalNixFlags,
) -> Result<ColdWorkspaceDiscovery, PrepareError> {
    let use_coalesced = load_tasks && nxr_nix::coalesced_discovery_available(&nix.version_banner);

    if use_coalesced {
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
        Some(
            nix.discover_tasks(flake_ref, nix_flags)
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
        let discovery = discover_workspace_with_cache(
            &context,
            DiscoveryCacheOptions {
                refresh: refresh_discovery,
                require_tasks: load_tasks,
            },
            || {
                let cold = cold_discover_workspace(&nix, &flake_ref, load_tasks, nix_flags)?;
                Ok::<WorkspaceDiscovery, PrepareError>(cold.discovery)
            },
        )?;
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
        let _timer = PlanPrepareGuard::start();
        document.validate().map_err(PrepareError::TaskSchema)?;
        let apps: Vec<App> = self.apps.values().cloned().collect();
        let mut nodes = BTreeMap::new();
        let mut upstream_keys = BTreeMap::new();
        let flake_root = self
            .flake
            .local_root
            .as_deref()
            .unwrap_or(self.invocation_directory.as_path());
        let mut digest_cache = nxr_core::RunDigestCache::new();
        for task_id in serial_order {
            let definition = document
                .tasks
                .get(task_id)
                .expect("execution plan only includes known task ids");
            let forwarded = if root_task_ids.iter().any(|id| id == task_id) {
                request_args
            } else {
                &[][..]
            };
            let app = resolve_app_by_name(&apps, definition.app.as_str())?;
            let execution_directory = resolve_task_execution_directory(
                &self.invocation_directory,
                &self.flake,
                root,
                cwd,
                definition.working_directory.as_deref(),
            )?;
            let mut context_name = None;
            let mut confirm = false;
            let effective_context = context_override.or(definition.context.as_deref());
            let applied_context = effective_context
                .map(|name| apply_task_context(document, task_id, name, environment_policy))
                .transpose()?;
            let context_shell = applied_context
                .as_ref()
                .and_then(|applied| applied.shell.clone());
            if let Some(applied) = &applied_context {
                context_name = Some(applied.context_name.clone());
                confirm = applied.confirm;
            }
            let node_environment = if let Some(applied) = &applied_context {
                applied.environment_policy.clone()
            } else {
                environment_policy.clone()
            };
            let effective_shell =
                resolve_effective_shell(shell, context_shell, definition.shell.clone());
            if let Some(shell_name) = effective_shell_wrap(effective_shell.as_deref(), shell_mode)
                && !self.dev_shells.contains(shell_name)
            {
                return Err(PrepareError::UnknownDevShell {
                    task: task_id.clone(),
                    shell: shell_name.to_owned(),
                });
            }
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
                &self.flake,
                &self.nix,
                app,
                &self.invocation_directory,
                &execution_directory,
                &strip_one_separator(forwarded),
            )?;
            if let Some(applied) = applied_context.as_ref() {
                plan.context = Some(applied.context_name.clone());
                plan.secrets = plan_secrets_for_core(&applied.plan_secrets);
                plan.context_env_set = applied.spawn_env_set.clone();
                plan.environment_policy = applied.environment_policy.clone();
            }
            let timeout = definition
                .timeout
                .as_deref()
                .map(nxr_task::parse_duration)
                .transpose()
                .map_err(|error| {
                    PrepareError::TaskSchema(SchemaError::InvalidTimeout {
                        task: task_id.clone(),
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
                        task: task_id.clone(),
                        message: error.to_string(),
                    })
                })?;
            let workspace_cache = build_workspace_cache_plan(
                document,
                task_id,
                definition,
                &self.nix.system,
                flake_root,
                execution_directory.as_str(),
                &upstream_keys,
                &WorkspaceCachePlanOptions {
                    forwarded_args: forwarded.to_vec(),
                    command_program: Some(self.nix.nix.to_string()),
                    command_argv: plan.command.arguments.clone(),
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
                Some(&mut digest_cache),
            )
            .map_err(PrepareError::WorkspaceCache)?;
            if let Some(key) = workspace_cache.action_key.as_ref() {
                upstream_keys.insert(task_id.clone(), key.clone());
            }
            nodes.insert(
                task_id.clone(),
                PreparedTaskNode {
                    id: task_id.clone(),
                    program: self.nix.nix.clone(),
                    arguments: plan.command.arguments.clone(),
                    cwd: execution_directory,
                    environment: node_environment,
                    plan,
                    timeout,
                    termination_grace,
                    context_name,
                    confirm,
                    workspace_cache: Some(workspace_cache),
                    flake_root: flake_root.to_path_buf(),
                },
            );
        }
        Ok(nodes)
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

fn resolve_execution_directory(
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
        command: PlanCommand {
            program: adapter.nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
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
        command: PlanCommand {
            program: nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
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
    let Some(hit) = lookup_prepared_plan(&key_digest, &fingerprints) else {
        return Ok(None);
    };
    if hit.prepare_kind != prepare_kind {
        return Ok(None);
    }
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
    let Some(hit) = lookup_prepared_plan(&key_digest, &fingerprints) else {
        return Ok(None);
    };
    if hit.prepare_kind != PlanPrepareKind::Fast {
        return Ok(None);
    }
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
        fingerprints,
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
    let discovery_inputs = match discovery_inputs_fingerprint(local_root, &[]) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let flake_lock = flake_lock_digest(local_root).ok().flatten();
    let nix_file_identity = nix_executable_identity(nix_path);
    Ok(Some(PlanCacheSharedFingerprints {
        nix_tree_fingerprint: nix_tree,
        discovery_inputs_fingerprint: discovery_inputs,
        flake_lock_digest: flake_lock,
        nix_path: nix_path.to_owned(),
        nix_version: nix_version.to_owned(),
        nix_file_identity,
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
}
