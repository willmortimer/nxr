//! Batched Nix store path queries (Wave 8b / ADR-0167).
//!
//! When [`crate::strategy::DiscoveryEvalPlan::batched_store_queries`] is true,
//! callers aggregate `nix path-info --json` instead of spawning one subprocess per
//! store path. On failure or when batching is disabled, callers fall back to
//! direct filesystem checks ([`nxr_core::store_exe_path_usable`]).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use camino::Utf8Path;
use serde_json::Value as JsonValue;

use crate::NixError;
use crate::capabilities::{NixFailureKind, run_nix};
use crate::capability_cache::detect_nix_environment;
use crate::strategy::{DiscoveryEvalPlan, plan_discovery_eval};
use nxr_core::{store_exe_path_usable, store_output_root_for_program};

/// Kill-switch forcing filesystem-only store checks (`fs`, `off`, `compat`).
pub const FORCE_FS_STORE_QUERIES_ENV: &str = "NXR_STORE_QUERIES";

/// Metadata for one `/nix/store` path from `nix path-info --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorePathInfo {
    pub path: String,
    pub registered: bool,
    pub references: Vec<String>,
}

/// Whether store path resolution should prefer batched `nix path-info --json`.
#[must_use]
pub fn prefer_batched_store_queries(plan: &DiscoveryEvalPlan) -> bool {
    plan.batched_store_queries
}

/// Whether batched store queries are enabled for the current host and env.
#[must_use]
pub fn batched_store_queries_enabled(plan: Option<&DiscoveryEvalPlan>) -> bool {
    if fs_store_queries_forced() {
        return false;
    }
    match plan {
        Some(plan) => prefer_batched_store_queries(plan),
        None => false,
    }
}

/// Whether batched store queries are enabled using the capability cache banner.
///
/// Falls back to filesystem-only checks when the cache probe fails.
#[must_use]
pub fn batched_store_queries_enabled_for_nix(nix: &Utf8Path) -> bool {
    if fs_store_queries_forced() {
        return false;
    }
    match detect_nix_environment(nix, false) {
        Ok(env) => {
            let plan = plan_discovery_eval(&env.version_banner, env.config_json.as_deref(), false);
            prefer_batched_store_queries(&plan)
        }
        Err(_) => false,
    }
}

/// Query multiple store paths in one `nix path-info --json` invocation.
///
/// Only `/nix/store/…` paths are queried; other inputs are ignored. Returns an
/// empty map when `paths` is empty or no valid store paths remain.
///
/// # Errors
///
/// Returns [`NixError`] when Nix fails or stdout is not valid JSON.
pub fn query_store_paths(
    nix: &Utf8Path,
    paths: &[String],
    cwd: Option<&Path>,
) -> Result<HashMap<String, StorePathInfo>, NixError> {
    let store_paths = dedupe_store_paths(paths);
    if store_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = vec!["path-info".to_owned(), "--json".to_owned()];
    args.extend(store_paths.iter().cloned());
    let stdout = run_nix_with_cwd(nix, &args, NixFailureKind::Capability, cwd)?;
    parse_path_info_json(&stdout, &store_paths)
}

/// Whether a cached store executable is still usable, preferring batched path-info.
///
/// When batching is enabled, verifies the store output root is still registered
/// before checking the program file on disk. Falls back to filesystem-only checks
/// on batch failure or when batching is disabled.
#[must_use]
pub fn store_exe_paths_usable(
    nix: &Utf8Path,
    program: &Path,
    store_output: Option<&str>,
    batched: bool,
    cwd: Option<&Path>,
) -> bool {
    if !batched {
        return store_exe_path_usable(program);
    }

    let program_text = program.to_string_lossy();
    let output_root = store_output
        .filter(|path| is_store_path(path))
        .map(str::to_owned)
        .or_else(|| store_output_root_for_program(program_text.as_ref()));

    if let Some(root) = output_root {
        match query_store_paths(nix, std::slice::from_ref(&root), cwd) {
            Ok(results) => match results.get(&root) {
                Some(info) if info.registered => store_exe_path_usable(program),
                _ => false,
            },
            Err(_) => store_exe_path_usable(program),
        }
    } else {
        store_exe_path_usable(program)
    }
}

/// Whether `path` is registered in the local Nix store via `nix path-info --json`.
///
/// Non-store paths return `false` without spawning Nix. Falls back to `Path::exists`
/// when batching is disabled or the query fails.
#[must_use]
pub fn store_path_registered(
    nix: &Utf8Path,
    path: &str,
    batched: bool,
    cwd: Option<&Path>,
) -> bool {
    if !is_store_path(path) {
        return false;
    }
    if !batched {
        return Path::new(path).exists();
    }
    match query_store_paths(nix, &[path.to_owned()], cwd) {
        Ok(results) => results.get(path).is_some_and(|info| info.registered),
        Err(_) => Path::new(path).exists(),
    }
}

fn fs_store_queries_forced() -> bool {
    fs_store_queries_forced_from(std::env::var(FORCE_FS_STORE_QUERIES_ENV).ok().as_deref())
}

fn fs_store_queries_forced_from(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("fs" | "off" | "compat" | "compatibility" | "0" | "false" | "no")
    )
}

fn is_store_path(path: &str) -> bool {
    path.starts_with("/nix/store/") && path.len() > "/nix/store/".len()
}

fn dedupe_store_paths(paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if is_store_path(path) && seen.insert(path.clone()) {
            out.push(path.clone());
        }
    }
    out
}

fn parse_path_info_json(
    stdout: &[u8],
    requested: &[String],
) -> Result<HashMap<String, StorePathInfo>, NixError> {
    let value: JsonValue =
        serde_json::from_slice(stdout).map_err(|source| NixError::InvalidJson { source })?;
    let mut out = HashMap::new();
    for path in requested {
        let entry = value.get(path);
        let registered = entry.is_some_and(|entry| !entry.is_null());
        let references = if registered {
            entry
                .and_then(|entry| entry.get("references"))
                .and_then(JsonValue::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        out.insert(
            path.clone(),
            StorePathInfo {
                path: path.clone(),
                registered,
                references,
            },
        );
    }
    Ok(out)
}

fn run_nix_with_cwd(
    nix: &Utf8Path,
    args: &[String],
    failure_kind: NixFailureKind,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, NixError> {
    if cwd.is_none() {
        return run_nix(nix, args, failure_kind);
    }
    nxr_core::record_nix_spawn();
    let mut command = std::process::Command::new(nix.as_std_path());
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let output = command.output().map_err(|source| NixError::SpawnFailed {
        nix: nix.to_path_buf(),
        source,
    })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(NixError::CommandFailed {
        nix: nix.to_path_buf(),
        args: args.to_vec(),
        status: output.status.code(),
        stderr,
        kind: failure_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        batched_store_queries_enabled, fs_store_queries_forced_from, parse_path_info_json,
        prefer_batched_store_queries, store_exe_paths_usable, store_path_registered,
    };
    use crate::capabilities::NixDistribution;
    use crate::determinate::probe_performance_features;
    use crate::strategy::{
        DiscoveryEvalPlan, DiscoveryEvalStrategy, plan_discovery_eval_with_overrides,
    };
    use camino::Utf8Path;
    use std::path::Path;

    const DETERMINATE_BANNER: &str = "nix (Determinate Nix 3.21.7) 2.34.8\n";

    #[test]
    fn parse_path_info_json_extracts_registration_and_references() {
        let json = r#"{
            "/nix/store/abc-hello": {
                "references": ["/nix/store/abc-hello", "/nix/store/def-dep"],
                "narSize": 42
            },
            "/nix/store/missing": null
        }"#;
        let paths = vec![
            "/nix/store/abc-hello".to_owned(),
            "/nix/store/missing".to_owned(),
        ];
        let parsed = parse_path_info_json(json.as_bytes(), &paths).expect("parse");
        let hello = parsed.get("/nix/store/abc-hello").expect("hello");
        assert!(hello.registered);
        assert_eq!(
            hello.references,
            vec![
                "/nix/store/abc-hello".to_owned(),
                "/nix/store/def-dep".to_owned(),
            ]
        );
        let missing = parsed.get("/nix/store/missing").expect("missing");
        assert!(!missing.registered);
        assert!(missing.references.is_empty());
    }

    #[test]
    fn fs_kill_switch_values_are_recognized() {
        assert!(fs_store_queries_forced_from(Some("fs")));
        assert!(fs_store_queries_forced_from(Some(" compat ")));
        assert!(!fs_store_queries_forced_from(Some("batched")));
    }

    #[test]
    fn prefer_batched_follows_discovery_eval_plan() {
        let distribution = NixDistribution::Determinate {
            product_version: Some("3.0.0".to_owned()),
        };
        let features = probe_performance_features(&distribution, None);
        let plan = DiscoveryEvalPlan {
            strategy: DiscoveryEvalStrategy::CoalescedParallelEval,
            features,
            use_coalesced_discovery: true,
            batched_store_queries: true,
            eval_worker_eligible: true,
        };
        assert!(prefer_batched_store_queries(&plan));
        assert!(batched_store_queries_enabled(Some(&plan)));
    }

    #[test]
    fn batched_enabled_follows_discovery_eval_plan() {
        let plan =
            plan_discovery_eval_with_overrides(DETERMINATE_BANNER, None, false, false, false);
        assert!(batched_store_queries_enabled(Some(&plan)));
        let disabled = plan_discovery_eval_with_overrides(
            DETERMINATE_BANNER,
            Some(r#"{"lazy-trees": {"value": false}}"#),
            false,
            false,
            false,
        );
        assert!(!batched_store_queries_enabled(Some(&disabled)));
    }

    #[test]
    fn store_path_registered_skips_nix_when_not_batched() {
        let nix = Utf8Path::new("/nix/bin/nix");
        assert!(!store_path_registered(nix, "/tmp/not-store", false, None));
        assert!(!store_path_registered(nix, "/nix/store/", false, None));
    }

    #[test]
    fn store_exe_paths_usable_falls_back_without_batched() {
        let nix = Utf8Path::new("/nix/bin/nix");
        assert!(!store_exe_paths_usable(
            nix,
            Path::new("/nonexistent/program"),
            None,
            false,
            None,
        ));
    }
}
