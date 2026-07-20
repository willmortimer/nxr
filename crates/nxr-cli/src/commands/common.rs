//! Shared helpers for list / run / plan commands.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_completion::cache::{
    DiscoveryCacheOptions, DiscoveryContext, WorkspaceDiscovery, discover_workspace_with_cache,
};
use nxr_core::diagnostics::exit;
use nxr_core::{App, EnvironmentPolicy, Plan, PlanCommand, PlanKind};
use nxr_nix::{
    AppNotFoundError, NixAdapter, NixCapabilities, NixError, OptionalNixFlags,
    TESTED_NIX_SUPPORT_FLOOR, detect_capabilities, locate_nix, nix_develop_wrap_run_args,
    nix_run_args, resolve_app_by_name,
};
use nxr_task::{
    SchemaError, TaskDocument, WORKING_DIRECTORY_FLAKE_ROOT, WORKING_DIRECTORY_INVOCATION,
};

use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::shell_mode::{ShellMode, active_dev_shell, effective_shell_wrap};

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
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::NotFound(error) => error.exit_code(),
            Self::TaskDiscovery(error) => error.exit_code(),
            Self::TaskSchema(_) => nxr_core::diagnostics::exit::EVALUATION,
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
    let snapshot = WorkspaceSnapshot::load(
        request.flake_arg,
        request.nix_override,
        false,
        request.nix_flags,
    )?;
    snapshot.prepare_discovered_app(request)
}

/// Build a [`Plan`] for `nix run <flake>#<app>` without adapter probes.
///
/// Locates `nix` only (no `currentSystem` / capability probes) unless the user
/// requested Required flags (`--offline` / `--accept-flake-config`), which need
/// a one-shot capability check. Missing apps surface as Nix failures; callers
/// may optionally discover afterward for "did you mean?" suggestions when
/// stderr indicates an installable-resolution failure.
///
/// # Errors
///
/// Returns [`PrepareError`] when directories, flake selection, or Nix location fail.
pub fn prepare_fast_app_plan(request: &AppRequest<'_>) -> Result<PreparedPlan, PrepareError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let nix = locate_nix_path(request.nix_override)?;
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

    Ok(PreparedPlan {
        plan,
        nix,
        execution_directory,
    })
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
        let invocation_directory = current_invocation_directory()?;
        let flake = resolve_flake(flake_arg, &invocation_directory)?;
        let nix = build_adapter(nix_override)?;
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
                refresh: false,
                require_tasks: load_tasks,
            },
            || {
                let apps = nix
                    .discover_apps(&flake_ref, nix_flags)
                    .map_err(PrepareError::Nix)?;
                let tasks = if load_tasks {
                    Some(
                        nix.discover_tasks(&flake_ref, nix_flags)
                            .map_err(PrepareError::TaskDiscovery)?,
                    )
                } else {
                    None
                };
                Ok::<WorkspaceDiscovery, PrepareError>(WorkspaceDiscovery { apps, tasks })
            },
        )?;
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
    ) -> Result<BTreeMap<String, PreparedTaskNode>, PrepareError> {
        document.validate().map_err(PrepareError::TaskSchema)?;
        let apps: Vec<App> = self.apps.values().cloned().collect();
        let mut nodes = BTreeMap::new();
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
            let app_request = AppRequest {
                flake_arg: None,
                nix_override: None,
                app: definition.app.as_str(),
                args: forwarded,
                root,
                cwd,
                shell,
                shell_mode,
                environment_policy: environment_policy.clone(),
                nix_flags,
            };
            let plan = build_plan(
                &app_request,
                &self.flake,
                &self.nix,
                app,
                &self.invocation_directory,
                &execution_directory,
                &strip_one_separator(forwarded),
            )?;
            nodes.insert(
                task_id.clone(),
                PreparedTaskNode {
                    id: task_id.clone(),
                    program: self.nix.nix.clone(),
                    arguments: plan.command.arguments.clone(),
                    cwd: execution_directory,
                    environment: plan.environment_policy.clone(),
                    plan,
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
    match nix_override {
        Some(path) => {
            let nix = Utf8PathBuf::from(path);
            if !nix.is_file() {
                return Err(NixError::NixNotFound { path: nix });
            }
            NixAdapter::from_nix(nix)
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
        command: PlanCommand {
            program: nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
    })
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
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp path");
        std::fs::create_dir(flake_root.join("crates")).expect("crates dir");
        let outside = temp.path().parent().expect("parent").join("outside");
        std::fs::create_dir(&outside).expect("outside dir");

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
