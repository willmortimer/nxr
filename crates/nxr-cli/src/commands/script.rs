//! `nxr script` — run a local workspace script without a flake app leaf.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_completion::cached_workspace_best_effort;
use nxr_core::diagnostics::exit;
use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind, PlanSecretRef};
use nxr_nix::{NixError, OptionalNixFlags};
use nxr_task::{
    AppListingMetadata, ContextError, PlanSecretEntry, SecretDelivery, SecretProvider,
    TaskDocument, apply_task_context, authorize_secret_refs, enforce_context_confirm,
};

use crate::commands::common::{
    AppRequest, PrepareError, build_adapter, cold_discover_app_listings, cold_discover_workspace,
    current_invocation_directory, locate_nix_path, resolve_execution_directory,
    strip_one_separator,
};
use crate::commands::dev_env::resolve_script_spawn_with_dev_env;
use crate::commands::history;
use crate::commands::plan::{PlanRenderError, write_plan};
use crate::commands::secrets::{
    SpawnSecrets, load_runtime_secret_config, prepare_spawn_secrets, project_identity,
};
use crate::commands::trust;
use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};
use crate::osc52::maybe_emit_app_failure_clipboard;
use crate::runner_output::RunnerOutput;
use crate::shell_mode::{ShellMode, active_dev_shell, resolve_effective_shell};

/// Convention directory under the flake root for named scripts.
pub const SCRIPT_CONVENTION_DIR: &str = ".nxr/scripts";

/// Extensions tried after an exact name miss under [SCRIPT_CONVENTION_DIR].
pub const SCRIPT_NAME_EXTENSIONS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "nu", "py", "rb", "js", "ts", "mjs",
];

/// Inputs for `nxr script`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
    pub path_or_name: &'a str,
    pub args: &'a [String],
    pub root: bool,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_mode: ShellMode,
    pub environment_policy: EnvironmentPolicy,
    pub nix_flags: &'a OptionalNixFlags,
    /// When set, applies a named execution context (schema v2).
    pub context: Option<&'a str>,
}

/// Prepared workspace-script execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedScript {
    pub plan: Plan,
    pub program: Utf8PathBuf,
    pub arguments: Vec<String>,
    pub execution_directory: Utf8PathBuf,
    pub script_path: Utf8PathBuf,
    pub confirm: bool,
    pub runtime_path: Option<String>,
    pub flake_display: String,
    pub local_root: Option<Utf8PathBuf>,
}

/// Errors while resolving or running a workspace script.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    Plan(#[from] PlanRenderError),
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error(transparent)]
    Trust(#[from] crate::commands::trust::TrustCommandError),
    #[error("workspace scripts require a local flake checkout (got remote ref {reference})")]
    RemoteFlake { reference: String },
    #[error("script not found: {path}")]
    NotFound { path: String },
    #[error("script is not executable and has no shebang: {path}")]
    NotExecutable { path: String },
    #[error("invalid shebang in {path}: {message}")]
    InvalidShebang { path: String, message: String },
    #[error("convention script name must not contain path separators: {name}")]
    InvalidConventionName { name: String },
    #[error("ambiguous convention script name {name}: multiple matches ({paths})")]
    AmbiguousConventionName { name: String, paths: String },
    #[error("failed to supervise child process: {0}")]
    Supervision(#[source] io::Error),
}

impl ScriptError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Prepare(error) => error.exit_code(),
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::Plan(_) => exit::EVALUATION,
            Self::Context(_) => exit::EVALUATION,
            Self::Trust(error) => error.exit_code(),
            Self::RemoteFlake { .. } | Self::NotFound { .. } => exit::NOT_FOUND,
            Self::NotExecutable { .. }
            | Self::InvalidShebang { .. }
            | Self::InvalidConventionName { .. }
            | Self::AmbiguousConventionName { .. } => exit::USAGE,
            Self::Supervision(_) => exit::PROCESS_SUPERVISION,
        }
    }
}

/// Resolve, optionally print a plan (`dry_run`), or execute a workspace script.
///
/// # Errors
///
/// Returns [`ScriptError`] when resolution, planning, or supervision fails.
pub fn execute(
    request: &ScriptRequest<'_>,
    dry_run: bool,
    json: bool,
    runner: RunnerOutput,
) -> Result<i32, ScriptError> {
    let started = std::time::Instant::now();
    let prepared = prepare_script(request)?;

    if dry_run {
        let mut stdout = io::stdout().lock();
        write_plan(&mut stdout, &prepared.plan, json)?;
        return Ok(exit::SUCCESS);
    }

    let code = execute_prepared_script(&prepared, "workspace-script", runner)?;

    history::record_completed_run(
        started,
        nxr_core::RunTargetKind::WorkspaceScript,
        prepared.script_path.as_str().to_owned(),
        Some(prepared.plan.flake.clone()),
        code,
        None,
        false,
    );

    maybe_emit_app_failure_clipboard(prepared.script_path.as_str(), code);

    Ok(code)
}

/// Prepare a workspace-script plan without executing.
///
/// # Errors
///
/// Returns [`ScriptError`] when the flake is remote or the script cannot be resolved.
pub fn prepare_script(request: &ScriptRequest<'_>) -> Result<PreparedScript, ScriptError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let local_root = flake
        .local_root
        .clone()
        .ok_or_else(|| ScriptError::RemoteFlake {
            reference: flake.display.clone(),
        })?;

    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let script_path = resolve_script_path(request.path_or_name, &invocation_cwd, &local_root)?;
    let forwarded = strip_one_separator(request.args);
    // Keep the no-context script path Nix-free: context metadata is unused unless
    // `--context` is set, so skip cold task-document discovery on the lean path.
    let context_fields = match request.context {
        Some(name) => {
            let document =
                load_task_document(&flake, &local_root, request.nix_override, request.nix_flags)?;
            resolve_script_context(&document, Some(name), &request.environment_policy)?
        }
        None => resolve_script_context(
            &TaskDocument::new(BTreeMap::new()),
            None,
            &request.environment_policy,
        )?,
    };
    let shell = resolve_effective_shell(request.shell, context_fields.shell.clone(), None);
    let spawn = resolve_script_spawn(&script_path, None)?;
    let nix = locate_nix_path(request.nix_override)?;

    let resolved = wrap_script_spawn(
        &flake,
        &nix,
        &local_root,
        shell.as_deref(),
        request.shell_mode,
        &context_fields.environment_policy,
        request.nix_flags,
        &spawn,
        &forwarded,
    )?;

    let plan = Plan {
        schema_version: Plan::SCHEMA_VERSION,
        kind: PlanKind::WorkspaceScript,
        flake: flake.nix_ref.clone(),
        system: "local".to_owned(),
        target: script_path
            .file_name()
            .unwrap_or(script_path.as_str())
            .to_owned(),
        attr_path: format!("workspace-script:{}", script_path),
        invocation_directory: invocation_cwd.as_str().to_owned(),
        execution_directory: execution_directory.as_str().to_owned(),
        shell: shell.clone(),
        active_shell: active_dev_shell(),
        environment_policy: resolved.environment_policy.clone(),
        context: context_fields.context,
        secrets: context_fields.secrets,
        context_env_set: context_fields.context_env_set,
        parameters: Vec::new(),
        matrix: Vec::new(),
        command: PlanCommand {
            program: resolved.program.as_str().to_owned(),
            arguments: resolved.arguments.clone(),
        },
        forwarded_arguments: forwarded,
        workspace_script: Some(script_path.as_str().to_owned()),
        mutable_source: true,
        fallback_app: None,
        environment_mode: resolved.environment_mode.clone(),
    };

    Ok(PreparedScript {
        plan,
        program: resolved.program,
        arguments: resolved.arguments,
        execution_directory,
        script_path,
        confirm: context_fields.confirm,
        runtime_path: None,
        flake_display: flake.display.clone(),
        local_root: Some(local_root),
    })
}

/// Resolved program + argv prefix for a script file (before forwarded args / shell wrap).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptSpawn {
    pub program: Utf8PathBuf,
    pub prefix_args: Vec<String>,
}

/// Resolve how to spawn `script_path`, optionally forcing an interpreter.
///
/// # Errors
///
/// Returns [`ScriptError`] when the file is missing or cannot be executed.
pub fn resolve_script_spawn(
    script_path: &Utf8Path,
    interpreter: Option<&str>,
) -> Result<ScriptSpawn, ScriptError> {
    if !script_path.is_file() {
        return Err(ScriptError::NotFound {
            path: script_path.as_str().to_owned(),
        });
    }

    if let Some(interp) = interpreter {
        return Ok(ScriptSpawn {
            program: Utf8PathBuf::from(interp),
            prefix_args: vec![script_path.as_str().to_owned()],
        });
    }

    if is_executable(script_path.as_std_path()) {
        return Ok(ScriptSpawn {
            program: script_path.to_path_buf(),
            prefix_args: Vec::new(),
        });
    }

    if let Some(shebang) = read_shebang(script_path)? {
        let mut parts = shebang.into_iter();
        let program = parts.next().ok_or_else(|| ScriptError::InvalidShebang {
            path: script_path.as_str().to_owned(),
            message: "empty shebang".to_owned(),
        })?;
        let mut prefix_args: Vec<String> = parts.collect();
        prefix_args.push(script_path.as_str().to_owned());
        return Ok(ScriptSpawn {
            program: Utf8PathBuf::from(program),
            prefix_args,
        });
    }

    Err(ScriptError::NotExecutable {
        path: script_path.as_str().to_owned(),
    })
}

fn wrap_script_spawn(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    local_root: &Utf8Path,
    shell: Option<&str>,
    shell_mode: ShellMode,
    environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
    spawn: &ScriptSpawn,
    forwarded: &[String],
) -> Result<crate::commands::dev_env::ResolvedScriptSpawn, ScriptError> {
    resolve_script_spawn_with_dev_env(
        flake,
        nix,
        local_root,
        shell,
        shell_mode,
        environment_policy,
        nix_flags,
        spawn,
        forwarded,
    )
    .map_err(ScriptError::Nix)
}

/// Resolve `path_or_name` to an absolute script path.
///
/// # Errors
///
/// Returns [`ScriptError`] when the path cannot be resolved.
pub fn resolve_script_path(
    path_or_name: &str,
    invocation_cwd: &Utf8Path,
    flake_root: &Utf8Path,
) -> Result<Utf8PathBuf, ScriptError> {
    if is_path_form(path_or_name) {
        let joined = if Path::new(path_or_name).is_absolute() {
            Utf8PathBuf::from(path_or_name)
        } else {
            invocation_cwd.join(path_or_name)
        };
        let resolved = joined.canonicalize_utf8().unwrap_or(joined);
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(ScriptError::NotFound {
            path: path_or_name.to_owned(),
        });
    }

    if path_or_name.contains('/') || path_or_name.contains('\\') {
        return Err(ScriptError::InvalidConventionName {
            name: path_or_name.to_owned(),
        });
    }

    let dir = flake_root.join(SCRIPT_CONVENTION_DIR);
    let exact = dir.join(path_or_name);
    if exact.is_file() {
        return Ok(exact.canonicalize_utf8().unwrap_or(exact));
    }

    if !path_or_name.contains('.') {
        let mut matches = Vec::new();
        for ext in SCRIPT_NAME_EXTENSIONS {
            let candidate = dir.join(format!("{path_or_name}.{ext}"));
            if candidate.is_file() {
                matches.push(candidate);
            }
        }
        match matches.len() {
            0 => {}
            1 => {
                let candidate = &matches[0];
                return Ok(candidate
                    .canonicalize_utf8()
                    .unwrap_or_else(|_| candidate.clone()));
            }
            _ => {
                let paths = matches
                    .iter()
                    .map(|path| path.as_str().to_owned())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ScriptError::AmbiguousConventionName {
                    name: path_or_name.to_owned(),
                    paths,
                });
            }
        }
    }

    Err(ScriptError::NotFound {
        path: format!("{SCRIPT_CONVENTION_DIR}/{path_or_name}"),
    })
}

fn is_path_form(token: &str) -> bool {
    token.contains('/')
        || token.contains('\\')
        || token.starts_with('.')
        || Path::new(token).is_absolute()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn read_shebang(path: &Utf8Path) -> Result<Option<Vec<String>>, ScriptError> {
    let mut file = fs::File::open(path.as_std_path()).map_err(ScriptError::Supervision)?;
    let mut buf = [0_u8; 512];
    let n = file.read(&mut buf).map_err(ScriptError::Supervision)?;
    if n < 2 || buf[0] != b'#' || buf[1] != b'!' {
        return Ok(None);
    }
    let line_end = buf[..n].iter().position(|&b| b == b'\n').unwrap_or(n);
    let line = std::str::from_utf8(&buf[2..line_end]).map_err(|_| ScriptError::InvalidShebang {
        path: path.as_str().to_owned(),
        message: "shebang is not valid UTF-8".to_owned(),
    })?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    // Support `#!/usr/bin/env bash`, `#!/usr/bin/env -S uv run`, and `#!/bin/sh`.
    let mut parts: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    if parts.first().map(String::as_str) == Some("/usr/bin/env") {
        parts.remove(0);
        if parts.first().map(String::as_str) == Some("-S") {
            parts.remove(0);
            if parts.is_empty() {
                return Err(ScriptError::InvalidShebang {
                    path: path.as_str().to_owned(),
                    message: "/usr/bin/env -S requires a command".to_owned(),
                });
            }
        }
    }
    Ok(Some(parts))
}

/// Outcome of resolving a live file-backed app fast path (ADR-0170).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveFastPathOutcome {
    Hit(Box<PreparedScript>),
    Miss { reasons: Vec<String> },
}

/// One convention script entry under `.nxr/scripts`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ConventionScriptEntry {
    pub name: String,
    pub path: String,
}

/// Resolve whether `nxr <app>` should spawn a live workspace script.
///
/// When `allow_cold_eval` is true, performs one targeted `nxr` / `nxrMetadata`
/// eval on warm discovery cache miss.
///
/// # Errors
///
/// Returns [`ScriptError`] when flake resolution or cold listing eval fails.
pub fn resolve_live_file_backed_app(
    request: &AppRequest<'_>,
    allow_cold_eval: bool,
) -> Result<LiveFastPathOutcome, ScriptError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd).map_err(ScriptError::from)?;
    let Some(local_root) = flake.local_root.as_ref() else {
        return Ok(LiveFastPathOutcome::Miss {
            reasons: vec!["remote flake: live fast path requires a local checkout".to_owned()],
        });
    };

    let mut miss_reasons = Vec::new();
    let listings = match warm_app_listings(local_root) {
        Some(apps) => apps,
        None => {
            miss_reasons.push("discovery cache miss for nxr.apps listing metadata".to_owned());
            if !allow_cold_eval {
                return Ok(LiveFastPathOutcome::Miss {
                    reasons: miss_reasons,
                });
            }
            // Keep the bare-app lean path (no capability probes / listing eval)
            // when the checkout cannot declare live fast-path metadata.
            if !local_flake_may_declare_live_fast_path(local_root) {
                miss_reasons.push(
                    "flake.nix has no live fast-path markers (workspace_path/fastPath)".to_owned(),
                );
                return Ok(LiveFastPathOutcome::Miss {
                    reasons: miss_reasons,
                });
            }
            let nix = build_adapter(request.nix_override).map_err(ScriptError::Nix)?;
            match cold_discover_app_listings(
                &nix,
                &flake.nix_ref,
                Some(local_root),
                request.nix_flags,
            )? {
                Some(apps) => apps,
                None => {
                    miss_reasons
                        .push("cold nxr listing eval returned no nxr.apps metadata".to_owned());
                    return Ok(LiveFastPathOutcome::Miss {
                        reasons: miss_reasons,
                    });
                }
            }
        }
    };

    let Some(listing) = listings.get(request.app) else {
        miss_reasons.push(format!(
            "app `{}` has no nxr.apps listing metadata",
            request.app
        ));
        return Ok(LiveFastPathOutcome::Miss {
            reasons: miss_reasons,
        });
    };

    resolve_listing_to_prepared(
        request,
        &flake,
        local_root,
        &invocation_cwd,
        listing,
        miss_reasons,
    )
}

fn warm_app_listings(local_root: &Utf8Path) -> Option<BTreeMap<String, AppListingMetadata>> {
    let cached = cached_workspace_best_effort(local_root)?;
    let document = cached.tasks.as_ref()?;
    if document.apps.is_empty() {
        return None;
    }
    Some(document.apps.clone())
}

/// Cheap local gate before opportunistic cold listing eval.
///
/// Avoids capability probes + `nix eval` on bare `nxr <app>` for ordinary
/// flakes that cannot express ADR-0170 live fast-path metadata.
fn local_flake_may_declare_live_fast_path(local_root: &Utf8Path) -> bool {
    let path = local_root.join("flake.nix");
    let Ok(contents) = fs::read_to_string(path.as_std_path()) else {
        // Unreadable: prefer attempting cold eval over silently skipping.
        return true;
    };
    contents.contains("workspace_path")
        || contents.contains("fastPath")
        || contents.contains("nxr.apps")
}

fn resolve_listing_to_prepared(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    local_root: &Utf8Path,
    invocation_cwd: &Utf8Path,
    listing: &AppListingMetadata,
    mut miss_reasons: Vec<String>,
) -> Result<LiveFastPathOutcome, ScriptError> {
    let Some(fast_path) = listing.fast_path.as_ref() else {
        miss_reasons.push("fastPath metadata is absent".to_owned());
        return Ok(LiveFastPathOutcome::Miss {
            reasons: miss_reasons,
        });
    };
    if !fast_path.enable {
        miss_reasons.push("fastPath.enable is false".to_owned());
        return Ok(LiveFastPathOutcome::Miss {
            reasons: miss_reasons,
        });
    }
    let Some(workspace_path) = listing.workspace_path.as_deref() else {
        miss_reasons.push("workspace_path is absent in nxr.apps listing".to_owned());
        return Ok(LiveFastPathOutcome::Miss {
            reasons: miss_reasons,
        });
    };

    let script_path = local_root.join(workspace_path);
    if !script_path.is_file() {
        miss_reasons.push(format!(
            "workspace script file is missing: {}",
            script_path.as_str()
        ));
        return Ok(LiveFastPathOutcome::Miss {
            reasons: miss_reasons,
        });
    }

    let execution_directory =
        resolve_execution_directory(invocation_cwd, flake, request.root, request.cwd)?;
    let forwarded = strip_one_separator(request.args);
    let prepared = prepare_live_file_app(
        request.shell,
        request.shell_mode,
        request.environment_policy.clone(),
        request.nix_override,
        request.nix_flags,
        request.context,
        flake,
        local_root,
        invocation_cwd,
        &execution_directory,
        request.app,
        workspace_path,
        listing.interpreter.as_deref(),
        fast_path.shell.as_deref(),
        listing.runtime_path.as_deref(),
        &forwarded,
    )?;
    Ok(LiveFastPathOutcome::Hit(Box::new(prepared)))
}

/// List convention scripts in `.nxr/scripts` for a local flake checkout.
///
/// # Errors
///
/// Returns [`ScriptError`] when the flake is remote or paths cannot be read.
pub fn list_convention_scripts(
    flake_arg: Option<&str>,
    nix_override: Option<&str>,
) -> Result<Vec<ConventionScriptEntry>, ScriptError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(flake_arg, &invocation_cwd).map_err(ScriptError::from)?;
    let local_root = flake
        .local_root
        .clone()
        .ok_or_else(|| ScriptError::RemoteFlake {
            reference: flake.display.clone(),
        })?;
    let _ = nix_override; // flake resolution only; keeps CLI symmetry with other commands

    let scripts_dir = local_root.join(SCRIPT_CONVENTION_DIR);
    if !scripts_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(scripts_dir.as_std_path()).map_err(ScriptError::Supervision)? {
        let entry = entry.map_err(ScriptError::Supervision)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .unwrap_or_default();
        if file_name.starts_with('.') {
            continue;
        }
        let relative = format!("{SCRIPT_CONVENTION_DIR}/{file_name}");
        let name = convention_name_from_file(&file_name);
        entries.push(ConventionScriptEntry {
            name,
            path: relative,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// Print convention script entries as plain text or JSON.
///
/// # Errors
///
/// Returns [`ScriptError`] on I/O or serialization failures.
pub fn write_convention_script_list(
    entries: &[ConventionScriptEntry],
    json: bool,
) -> Result<(), ScriptError> {
    let mut stdout = io::stdout().lock();
    if json {
        let rendered = serde_json::to_string_pretty(entries).map_err(|error| {
            ScriptError::Plan(crate::commands::plan::PlanRenderError::Json(error))
        })?;
        writeln!(stdout, "{rendered}").map_err(ScriptError::Supervision)?;
    } else {
        for entry in entries {
            writeln!(stdout, "{}  {}", entry.name, entry.path).map_err(ScriptError::Supervision)?;
        }
    }
    Ok(())
}

fn convention_name_from_file(file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name);
    stem.to_owned()
}

/// Prepare a live file-backed app spawn (ADR-0170 fast path).
///
/// # Errors
///
/// Returns [`ScriptError`] when the workspace file cannot be spawned.
pub fn prepare_live_file_app(
    request_shell: Option<&str>,
    request_shell_mode: ShellMode,
    environment_policy: EnvironmentPolicy,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
    context_name: Option<&str>,
    flake: &FlakeSelection,
    local_root: &Utf8Path,
    invocation_cwd: &Utf8Path,
    execution_directory: &Utf8Path,
    app_name: &str,
    workspace_path: &str,
    interpreter: Option<&str>,
    fast_path_shell: Option<&str>,
    runtime_path: Option<&str>,
    forwarded: &[String],
) -> Result<PreparedScript, ScriptError> {
    validate_repo_relative_file(workspace_path)?;
    let script_path = local_root.join(workspace_path);
    let spawn = resolve_script_spawn(&script_path, interpreter)?;
    let nix = locate_nix_path(nix_override)?;
    // Live fast path already resolved listing metadata; only load the task
    // document when `--context` requires it.
    let context_fields = match context_name {
        Some(name) => {
            let document = load_task_document(flake, local_root, nix_override, nix_flags)?;
            resolve_script_context(&document, Some(name), &environment_policy)?
        }
        None => resolve_script_context(
            &TaskDocument::new(BTreeMap::new()),
            None,
            &environment_policy,
        )?,
    };
    let shell = resolve_effective_shell(
        request_shell,
        context_fields.shell.clone(),
        fast_path_shell.map(str::to_owned),
    );
    let resolved = wrap_script_spawn(
        flake,
        &nix,
        local_root,
        shell.as_deref(),
        request_shell_mode,
        &context_fields.environment_policy,
        nix_flags,
        &spawn,
        forwarded,
    )?;

    let plan = Plan {
        schema_version: Plan::SCHEMA_VERSION,
        kind: PlanKind::WorkspaceScript,
        flake: flake.nix_ref.clone(),
        system: "local".to_owned(),
        target: app_name.to_owned(),
        attr_path: format!("apps.local.{app_name}"),
        invocation_directory: invocation_cwd.as_str().to_owned(),
        execution_directory: execution_directory.as_str().to_owned(),
        shell: shell.clone(),
        active_shell: active_dev_shell(),
        environment_policy: resolved.environment_policy.clone(),
        context: context_fields.context,
        secrets: context_fields.secrets,
        context_env_set: context_fields.context_env_set,
        parameters: Vec::new(),
        matrix: Vec::new(),
        command: PlanCommand {
            program: resolved.program.as_str().to_owned(),
            arguments: resolved.arguments.clone(),
        },
        forwarded_arguments: forwarded.to_vec(),
        workspace_script: Some(script_path.as_str().to_owned()),
        mutable_source: true,
        fallback_app: Some(app_name.to_owned()),
        environment_mode: resolved.environment_mode.clone(),
    };

    Ok(PreparedScript {
        plan,
        program: resolved.program,
        arguments: resolved.arguments,
        execution_directory: execution_directory.to_path_buf(),
        script_path,
        confirm: context_fields.confirm,
        runtime_path: runtime_path.map(str::to_owned),
        flake_display: flake.display.clone(),
        local_root: Some(local_root.to_path_buf()),
    })
}

/// Execute a prepared workspace or live file-backed script in the foreground.
///
/// # Errors
///
/// Returns [`ScriptError`] when trust, confirmation, secret resolution, or spawn fails.
pub fn execute_prepared_script(
    prepared: &PreparedScript,
    confirm_label: &str,
    runner: RunnerOutput,
) -> Result<i32, ScriptError> {
    if prepared.requires_project_trust() {
        trust::enforce_for_execution(
            &prepared.flake_display,
            prepared.local_root.as_deref(),
            &prepared.plan.flake,
        )?;
    }

    if prepared.confirm {
        let context_name = prepared.plan.context.as_deref().unwrap_or("unknown");
        enforce_context_confirm(context_name, confirm_label, true)?;
    }

    runner
        .verbose(format!(
            "running workspace script {} from {}",
            prepared.script_path, prepared.plan.flake
        ))
        .map_err(ScriptError::Supervision)?;

    let (env_overrides, _stdin_payload, _spawn_secrets) = build_script_spawn_env(prepared)?;
    let (code, _stderr) = nxr_process::run_in_with_stderr(
        prepared.program.as_std_path(),
        &prepared.arguments,
        Some(prepared.execution_directory.as_std_path()),
        &prepared.plan.environment_policy,
        env_overrides.as_ref(),
    )
    .map_err(ScriptError::Supervision)?;

    Ok(code)
}

impl PreparedScript {
    #[must_use]
    pub fn requires_project_trust(&self) -> bool {
        self.confirm || !self.plan.secrets.is_empty()
    }
}

struct ResolvedScriptContext {
    environment_policy: EnvironmentPolicy,
    context: Option<String>,
    secrets: Vec<PlanSecretRef>,
    context_env_set: BTreeMap<String, String>,
    shell: Option<String>,
    confirm: bool,
}

fn resolve_script_context(
    document: &TaskDocument,
    context_name: Option<&str>,
    base_policy: &EnvironmentPolicy,
) -> Result<ResolvedScriptContext, ScriptError> {
    let Some(name) = context_name else {
        return Ok(ResolvedScriptContext {
            environment_policy: base_policy.clone(),
            context: None,
            secrets: Vec::new(),
            context_env_set: BTreeMap::new(),
            shell: None,
            confirm: false,
        });
    };
    let applied = apply_task_context(document, "workspace-script", name, base_policy)?;
    Ok(ResolvedScriptContext {
        environment_policy: applied.environment_policy,
        context: Some(applied.context_name),
        secrets: plan_secrets_for_core(&applied.plan_secrets),
        context_env_set: applied.spawn_env_set,
        shell: applied.shell,
        confirm: applied.confirm,
    })
}

fn load_task_document(
    flake: &FlakeSelection,
    local_root: &Utf8Path,
    nix_override: Option<&str>,
    nix_flags: &OptionalNixFlags,
) -> Result<TaskDocument, ScriptError> {
    if let Some(cached) = cached_workspace_best_effort(local_root)
        && let Some(document) = cached.tasks
    {
        return Ok(document);
    }
    let nix = build_adapter(nix_override).map_err(ScriptError::Nix)?;
    let workspace = cold_discover_workspace(&nix, &flake.nix_ref, true, nix_flags)
        .map_err(ScriptError::Prepare)?;
    Ok(workspace
        .discovery
        .tasks
        .unwrap_or_else(|| TaskDocument::new(BTreeMap::new())))
}

fn build_script_spawn_env(
    prepared: &PreparedScript,
) -> Result<
    (
        Option<BTreeMap<String, String>>,
        Option<Vec<u8>>,
        SpawnSecrets,
    ),
    ScriptError,
> {
    let flake_root = prepared.script_path.parent().map(|path| path.as_std_path());
    let project_id = project_identity(flake_root.unwrap_or_else(|| Path::new(".")));
    let (user_config, secret_bindings) = load_runtime_secret_config()?;
    if !prepared.plan.secrets.is_empty() {
        authorize_script_secrets(&prepared.plan, &project_id, &user_config)?;
    }
    let entries = plan_secret_entries_from_core(&prepared.plan.secrets);
    let spawn_secrets =
        prepare_spawn_secrets(&entries, &project_id, &user_config, &secret_bindings)?;
    let mut merged = nxr_task::merge_spawn_env_overrides(
        &prepared.plan.context_env_set,
        &spawn_secrets.env_overrides,
    );
    if let Some(runtime_path) = prepend_runtime_path(prepared.runtime_path.as_deref()) {
        merged.insert("PATH".to_owned(), runtime_path);
    }
    let env_overrides = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };
    Ok((
        env_overrides,
        spawn_secrets.stdin_payload.clone(),
        spawn_secrets,
    ))
}

fn authorize_script_secrets(
    plan: &Plan,
    project_id: &str,
    user_config: &nxr_core::config::UserConfig,
) -> Result<(), ScriptError> {
    if user_config.trusted_projects.is_empty() {
        return Ok(());
    }
    let trust_refs: Vec<String> = plan
        .secrets
        .iter()
        .map(|secret| secret.reference.clone())
        .collect();
    authorize_secret_refs(project_id, &trust_refs, &user_config.trusted_projects)?;
    Ok(())
}

fn prepend_runtime_path(runtime_path: Option<&str>) -> Option<String> {
    let fragment = runtime_path.filter(|value| !value.is_empty())?;
    let current = std::env::var("PATH").unwrap_or_default();
    Some(if current.is_empty() {
        fragment.to_owned()
    } else {
        format!("{fragment}:{current}")
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

fn plan_secret_entries_from_core(secrets: &[PlanSecretRef]) -> Vec<PlanSecretEntry> {
    secrets
        .iter()
        .map(|secret| PlanSecretEntry {
            name: secret.name.clone(),
            reference: secret.reference.clone(),
            delivery: parse_plan_secret_delivery(&secret.delivery),
            provider: parse_plan_secret_provider(&secret.provider),
            value: nxr_task::PlanSecretValuePlaceholder::RUNTIME,
        })
        .collect()
}

fn parse_plan_secret_delivery(label: &str) -> SecretDelivery {
    match label {
        "file" => SecretDelivery::File,
        "stdin" => SecretDelivery::Stdin,
        _ => SecretDelivery::Env,
    }
}

fn parse_plan_secret_provider(label: &str) -> SecretProvider {
    match label {
        "file" => SecretProvider::File,
        "sops" => SecretProvider::Sops,
        "sops-nix" => SecretProvider::SopsNix,
        _ => SecretProvider::Env,
    }
}

fn secret_provider_label(provider: SecretProvider) -> &'static str {
    match provider {
        SecretProvider::Env => "env",
        SecretProvider::File => "file",
        SecretProvider::Sops => "sops",
        SecretProvider::SopsNix => "sops-nix",
    }
}

fn secret_delivery_label(delivery: SecretDelivery) -> &'static str {
    match delivery {
        SecretDelivery::Env => "env",
        SecretDelivery::File => "file",
        SecretDelivery::Stdin => "stdin",
    }
}

fn validate_repo_relative_file(path: &str) -> Result<(), ScriptError> {
    if path.is_empty() || Path::new(path).is_absolute() || path.split('/').any(|part| part == "..")
    {
        return Err(ScriptError::InvalidConventionName {
            name: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedScript, SCRIPT_CONVENTION_DIR, ScriptError, build_script_spawn_env, is_path_form,
        prepend_runtime_path, resolve_script_path, resolve_script_spawn,
    };
    use camino::Utf8PathBuf;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn path_form_detects_relative_and_absolute() {
        assert!(is_path_form("./scripts/hello.sh"));
        assert!(is_path_form("scripts/hello.sh"));
        assert!(is_path_form("/tmp/hello.sh"));
        assert!(!is_path_form("deploy"));
        assert!(!is_path_form("hello.sh"));
    }

    #[test]
    fn build_script_spawn_env_prepends_runtime_path() {
        use camino::Utf8PathBuf;
        use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind};

        let prepared = PreparedScript {
            plan: Plan {
                schema_version: Plan::SCHEMA_VERSION,
                kind: PlanKind::WorkspaceScript,
                flake: "path:/tmp".to_owned(),
                system: "local".to_owned(),
                target: "greet".to_owned(),
                attr_path: "workspace-script:greet".to_owned(),
                invocation_directory: "/tmp".to_owned(),
                execution_directory: "/tmp".to_owned(),
                shell: None,
                active_shell: None,
                environment_policy: EnvironmentPolicy::Inherit,
                context: None,
                secrets: Vec::new(),
                context_env_set: BTreeMap::new(),
                parameters: Vec::new(),
                matrix: Vec::new(),
                command: PlanCommand {
                    program: "/bin/sh".to_owned(),
                    arguments: Vec::new(),
                },
                forwarded_arguments: Vec::new(),
                workspace_script: None,
                mutable_source: true,
                fallback_app: None,
                environment_mode: None,
            },
            program: Utf8PathBuf::from("/bin/sh"),
            arguments: Vec::new(),
            execution_directory: Utf8PathBuf::from("/tmp"),
            script_path: Utf8PathBuf::from("/tmp/greet.sh"),
            confirm: false,
            runtime_path: Some("/nix/store/example/bin".to_owned()),
            flake_display: "path:/tmp".to_owned(),
            local_root: None,
        };
        let (env_overrides, _, _) = build_script_spawn_env(&prepared).expect("spawn env");
        let env = env_overrides.expect("overrides");
        assert!(
            env.get("PATH")
                .is_some_and(|path| path.starts_with("/nix/store/example/bin"))
        );
    }

    #[test]
    fn prepend_runtime_path_prefixes_fragment() {
        let merged = prepend_runtime_path(Some("/nix/store/bin")).expect("path");
        assert!(merged.starts_with("/nix/store/bin"));
    }

    #[test]
    fn convention_name_resolves_under_nxr_scripts() {
        let tmp = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let dir = root.join(SCRIPT_CONVENTION_DIR);
        fs::create_dir_all(&dir).expect("mkdir");
        let script = dir.join("deploy.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").expect("write");

        let resolved = resolve_script_path("deploy", &root, &root).expect("resolve");
        assert_eq!(
            resolved.canonicalize_utf8().unwrap_or(resolved.clone()),
            script.canonicalize_utf8().unwrap_or(script)
        );
    }

    #[test]
    fn convention_name_rejects_ambiguous_extension_matches() {
        let tmp = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let dir = root.join(SCRIPT_CONVENTION_DIR);
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("deploy.sh"), "#!/bin/sh\necho sh\n").expect("write sh");
        fs::write(dir.join("deploy.py"), "#!/bin/sh\necho py\n").expect("write py");

        let err = resolve_script_path("deploy", &root, &root).expect_err("ambiguous");
        assert!(matches!(err, ScriptError::AmbiguousConventionName { .. }));
    }

    #[test]
    fn shebang_env_split_string_runs_interpreter_and_args() {
        let tmp = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let script = root.join("runner");
        fs::write(&script, "#!/usr/bin/env -S uv run\n").expect("write");

        let spawn = resolve_script_spawn(&script, None).expect("spawn");
        assert_eq!(spawn.program.as_str(), "uv");
        assert_eq!(
            spawn.prefix_args,
            vec!["run".to_owned(), script.as_str().to_owned()]
        );
    }
}
