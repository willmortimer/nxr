//! Shared helpers for list / run / plan commands.

use std::io;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::diagnostics::exit;
use nxr_core::{App, EnvironmentPolicy, Plan, PlanCommand, PlanKind};
use nxr_nix::{
    AppNotFoundError, NixAdapter, NixError, detect_system, nix_develop_wrap_run_args, nix_run_args,
    resolve_app_by_name,
};

use crate::flake::{FlakeResolveError, FlakeSelection, resolve_flake};

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
    pub environment_policy: EnvironmentPolicy,
}

/// Inputs for flake discovery without a resolved app target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoverRequest<'a> {
    pub flake_arg: Option<&'a str>,
    pub nix_override: Option<&'a str>,
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
    #[error(transparent)]
    Flake(#[from] FlakeResolveError),
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error(transparent)]
    NotFound(#[from] AppNotFoundError),
}

impl PrepareError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::InvocationDirectory(_)
            | Self::NonUtf8InvocationDirectory
            | Self::RootRequiresLocalFlake => exit::DISCOVERY,
            Self::RootAndCwdConflict => exit::USAGE,
            Self::Flake(error) => error.exit_code(),
            Self::Nix(error) => error.exit_code(),
            Self::NotFound(error) => error.exit_code(),
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
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let adapter = build_adapter(request.nix_override)?;
    let apps = adapter.discover_apps(&flake.nix_ref)?;

    Ok(DiscoveredApps { apps })
}

/// Resolve invocation CWD, flake, apps, and build a [`Plan`].
///
/// # Errors
///
/// Returns [`PrepareError`] when directories, flake selection, discovery, or
/// app resolution fail.
pub fn prepare_app_plan(request: &AppRequest<'_>) -> Result<PreparedPlan, PrepareError> {
    let invocation_cwd = current_invocation_directory()?;
    let flake = resolve_flake(request.flake_arg, &invocation_cwd)?;
    let execution_directory =
        resolve_execution_directory(&invocation_cwd, &flake, request.root, request.cwd)?;
    let adapter = build_adapter(request.nix_override)?;
    let apps = adapter.discover_apps(&flake.nix_ref)?;
    let app = resolve_app_by_name(&apps, request.app)?;
    let forwarded = strip_one_separator(request.args);
    let plan = build_plan(
        request,
        &flake,
        &adapter,
        app,
        &invocation_cwd,
        &execution_directory,
        &forwarded,
    );

    Ok(PreparedPlan {
        plan,
        nix: adapter.nix,
        execution_directory,
    })
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
            let system = detect_system(&nix)?;
            Ok(NixAdapter::with_nix_and_system(nix, system))
        }
        None => NixAdapter::new(),
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

fn build_plan(
    request: &AppRequest<'_>,
    flake: &FlakeSelection,
    adapter: &NixAdapter,
    app: &App,
    invocation_directory: &Utf8Path,
    execution_directory: &Utf8Path,
    forwarded: &[String],
) -> Plan {
    let run_argv = nix_run_args(&flake.nix_ref, &app.name, forwarded);
    let command_arguments = match request.shell {
        Some(shell_name) => {
            nix_develop_wrap_run_args(adapter.nix.as_str(), &flake.nix_ref, shell_name, &run_argv)
        }
        None => run_argv,
    };

    Plan {
        schema_version: Plan::SCHEMA_VERSION,
        kind: PlanKind::App,
        flake: flake.nix_ref.clone(),
        system: adapter.system.clone(),
        target: app.name.clone(),
        attr_path: app.attr_path.clone(),
        invocation_directory: invocation_directory.as_str().to_owned(),
        execution_directory: execution_directory.as_str().to_owned(),
        shell: request.shell.map(str::to_owned),
        environment_policy: request.environment_policy.clone(),
        command: PlanCommand {
            program: adapter.nix.as_str().to_owned(),
            arguments: command_arguments,
        },
        forwarded_arguments: forwarded.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        AppRequest, PrepareError, build_plan, resolve_execution_directory, strip_one_separator,
    };
    use crate::flake::FlakeSelection;
    use nxr_core::App;
    use nxr_nix::NixAdapter;

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
        let request = AppRequest {
            flake_arg: None,
            nix_override: None,
            app: "hello",
            args: &["--".to_owned(), "one".to_owned()],
            root: false,
            cwd: None,
            shell: None,
            environment_policy: nxr_core::EnvironmentPolicy::Inherit,
        };
        let plan = build_plan(
            &request,
            &flake,
            &adapter,
            &app,
            camino::Utf8Path::new("/work"),
            camino::Utf8Path::new("/work"),
            &forwarded,
        );

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
        let request = AppRequest {
            flake_arg: None,
            nix_override: None,
            app: "shell-marker",
            args: &[],
            root: false,
            cwd: None,
            shell: Some("default"),
            environment_policy: nxr_core::EnvironmentPolicy::Inherit,
        };
        let plan = build_plan(
            &request,
            &flake,
            &adapter,
            &app,
            camino::Utf8Path::new("/work"),
            camino::Utf8Path::new("/work"),
            &[],
        );

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
