//! Coalesced cold discovery via a single `nix eval` when parallel eval is available.

use camino::Utf8Path;
use nxr_task::TaskDocument;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::capabilities::{NixFailureKind, run_nix};
use crate::determinate::distribution_from_version_banner;
use crate::discovery::{
    OutputTable, flake_show_has_nxr_for_system, parse_apps_from_flake_show,
    parse_outputs_from_flake_show,
};
use crate::tasks::{self, TaskDiscoveryError};
use crate::{NixError, ParseAppsError};

/// Environment variable forcing coalesced discovery (integration tests).
pub const FORCE_COALESCED_DISCOVERY_ENV: &str = "NXR_FORCE_COALESCED_DISCOVERY";

/// Result of a coalesced discovery eval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoalescedDiscovery {
    pub show: JsonValue,
    pub tasks: Option<TaskDocument>,
}

/// Whether coalesced discovery may be used for this Nix distribution.
#[must_use]
pub fn coalesced_discovery_available(version_banner: &str) -> bool {
    if std::env::var_os(FORCE_COALESCED_DISCOVERY_ENV).is_some() {
        return true;
    }
    distribution_from_version_banner(version_banner).is_determinate()
}

/// Build argv for the coalesced discovery `nix eval --json --expr …` invocation.
#[must_use]
pub fn coalesced_discovery_args(flake_ref: &str, system: &str) -> Vec<String> {
    vec![
        "eval".to_owned(),
        "--json".to_owned(),
        "--impure".to_owned(),
        "--expr".to_owned(),
        coalesced_discovery_expr(flake_ref, system),
    ]
}

/// Nix expression returning `{ inventory = <flake-show-shaped>; nxr = <task doc or null>; }`.
///
/// Inventory leaves are metadata-only so `nix eval --json` never serializes derivations.
#[must_use]
pub fn coalesced_discovery_expr(flake_ref: &str, system: &str) -> String {
    let flake_literal = nix_string_literal(flake_ref);
    let system_literal = nix_string_literal(system);
    format!(
        r#"
let
  flakeRef = {flake_literal};
  system = {system_literal};
  flake = builtins.getFlake flakeRef;
  leafType = name: if name == "apps" then "app" else "derivation";
  perSystem = name:
    let output = flake.${{name}} or null;
    in if output == null then null
       else if builtins.isFunction output then output.${{system}} or null
       else if builtins.isAttrs output && (output ? ${{system}}) then output.${{system}}
       else output;
  showTable = name:
    let table = perSystem name;
        kind = leafType name;
    in if table == null then null else {{
      "${{system}}" = builtins.mapAttrs (_: _: {{ type = kind; }}) table;
    }};
  inventory = builtins.removeAttrs {{
    apps = showTable "apps";
    packages = showTable "packages";
    checks = showTable "checks";
    devShells = showTable "devShells";
  }} (builtins.filter (n: (showTable n) == null) [ "apps" "packages" "checks" "devShells" ]);
  nxr =
    if flake ? nxr && flake.nxr ? ${{system}} then flake.nxr.${{system}}
    else null;
in {{
  inventory = inventory // (if nxr == null then {{}} else {{ nxr = {{ "${{system}}" = nxr; }}; }});
  nxr = nxr;
}}
"#
    )
}

fn nix_string_literal(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[derive(Debug, Deserialize)]
struct CoalescedEvalEnvelope {
    inventory: JsonValue,
    nxr: Option<JsonValue>,
}

/// Run coalesced discovery and normalize into apps, optional tasks, and dev shells.
///
/// # Errors
///
/// Returns [`NixError`] or [`TaskDiscoveryError`] when evaluation or parsing fails.
pub fn discover_coalesced(
    nix: &Utf8Path,
    _system: &str,
    _flake_ref: &str,
    args: &[String],
) -> Result<CoalescedDiscovery, CoalescedDiscoveryError> {
    let stdout = run_nix(nix, args, NixFailureKind::Evaluation)?;
    let envelope: CoalescedEvalEnvelope = serde_json::from_slice(&stdout)
        .map_err(|source| CoalescedDiscoveryError::InvalidEnvelope { source })?;
    Ok(CoalescedDiscovery {
        show: envelope.inventory,
        tasks: match envelope.nxr {
            Some(value) => Some(tasks::parse_task_document(&value)?),
            None => None,
        },
    })
}

/// Errors while running or parsing coalesced discovery.
#[derive(Debug, thiserror::Error)]
pub enum CoalescedDiscoveryError {
    #[error(transparent)]
    Nix(#[from] NixError),
    #[error("coalesced discovery output was not valid JSON: {source}")]
    InvalidEnvelope { source: serde_json::Error },
    #[error(transparent)]
    Tasks(#[from] TaskDiscoveryError),
    #[error(transparent)]
    ParseApps(#[from] ParseAppsError),
}

impl CoalescedDiscoveryError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        use nxr_core::diagnostics::exit;

        match self {
            Self::Nix(error) => error.exit_code(),
            Self::InvalidEnvelope { .. } | Self::Tasks(_) | Self::ParseApps(_) => exit::EVALUATION,
        }
    }
}

/// Parsed workspace data from a coalesced discovery eval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoalescedWorkspace {
    pub apps: Vec<nxr_core::App>,
    pub tasks: Option<TaskDocument>,
    pub dev_shells: Vec<String>,
    pub show: JsonValue,
}

impl CoalescedDiscovery {
    /// Normalize coalesced output into apps, tasks, and dev shell names.
    ///
    /// # Errors
    ///
    /// Returns [`CoalescedDiscoveryError`] when flake-show parsing fails.
    pub fn into_workspace(
        self,
        flake_ref: &str,
        system: &str,
        load_tasks: bool,
    ) -> Result<CoalescedWorkspace, CoalescedDiscoveryError> {
        let apps = parse_apps_from_flake_show(&self.show, flake_ref, system)?;
        let dev_shells =
            parse_outputs_from_flake_show(&self.show, flake_ref, system, OutputTable::DevShells)?
                .into_iter()
                .map(|shell| shell.name)
                .collect();
        let tasks = if load_tasks {
            Some(
                self.tasks
                    .unwrap_or_else(|| TaskDocument::new(std::collections::BTreeMap::new())),
            )
        } else if flake_show_has_nxr_for_system(&self.show, system) {
            None
        } else {
            Some(TaskDocument::new(std::collections::BTreeMap::new()))
        };
        Ok(CoalescedWorkspace {
            apps,
            tasks,
            dev_shells,
            show: self.show,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FORCE_COALESCED_DISCOVERY_ENV, coalesced_discovery_args, coalesced_discovery_available,
        coalesced_discovery_expr, discover_coalesced,
    };
    use crate::OptionalNixFlags;
    use crate::determinate::distribution_from_version_banner;

    #[test]
    fn coalesced_available_for_determinate_only() {
        assert!(coalesced_discovery_available(
            "nix (Determinate Nix 3.21.7) 2.34.8\n"
        ));
    }

    #[test]
    fn coalesced_unavailable_for_upstream_without_force_env() {
        if std::env::var_os(FORCE_COALESCED_DISCOVERY_ENV).is_some() {
            return;
        }
        assert!(!coalesced_discovery_available("nix (Nix) 2.34.7\n"));
    }

    #[test]
    fn coalesced_expr_embeds_flake_ref_and_system() {
        let expr = coalesced_discovery_expr("./fixtures/task-dag", "aarch64-darwin");
        assert!(expr.contains("./fixtures/task-dag"));
        assert!(expr.contains("aarch64-darwin"));
        assert!(expr.contains("builtins.getFlake"));
    }

    #[test]
    fn coalesced_args_use_eval_json_impure() {
        let args = coalesced_discovery_args(".", "x86_64-linux");
        assert_eq!(args[0], "eval");
        assert_eq!(args[1], "--json");
        assert_eq!(args[2], "--impure");
        assert!(args.iter().any(|arg| arg.contains("builtins.getFlake")));
    }

    #[test]
    fn distribution_gate_matches_determinate_banner() {
        assert!(
            distribution_from_version_banner("nix (Determinate Nix 3.0.0) 2.34.0\n")
                .is_determinate()
        );
        assert!(!distribution_from_version_banner("nix (Lix, like Nix) 2.91.0").is_determinate());
    }

    #[test]
    fn discover_coalesced_task_dag_fixture() {
        if std::env::var_os("NXR_SKIP_NIX_INTEGRATION").is_some() {
            return;
        }
        let Some(nix_path) = which::which("nix").ok() else {
            eprintln!("skipping: nix not on PATH");
            return;
        };
        let nix = camino::Utf8PathBuf::from_path_buf(nix_path).expect("utf-8 path");
        let adapter = crate::NixAdapter::new().expect("adapter");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../fixtures/task-dag");
        let flake_ref = format!(
            "path:{}",
            fixture.canonicalize().expect("fixture").display()
        );
        let args = coalesced_discovery_args(&flake_ref, &adapter.system);
        let mut flags = OptionalNixFlags::default();
        flags.no_write_lock_file = true;
        let args = adapter
            .compatible_argv(args, &flags)
            .expect("compatible argv");
        let coalesced =
            discover_coalesced(&nix, &adapter.system, &flake_ref, &args).expect("coalesced eval");
        let workspace = coalesced
            .into_workspace(&flake_ref, &adapter.system, true)
            .expect("workspace");
        assert!(!workspace.apps.is_empty());
        assert!(
            workspace
                .tasks
                .as_ref()
                .is_some_and(|doc| !doc.tasks.is_empty())
        );
    }
}
