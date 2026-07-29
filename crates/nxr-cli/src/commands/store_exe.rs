//! Resolve foreground/task app spawns via optional store-exe cache (ADR-0153).

use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_completion::{
    discovery_inputs_fingerprint, hint_discovery_inputs_for_root, nix_tree_fingerprint,
};
use nxr_core::cas::flake_lock_digest;
use nxr_core::{
    PLAN_SECRET_RUNTIME_PLACEHOLDER, Plan, PlanCacheSharedFingerprints, PlanSecretRef,
    StoreExeCacheKeyMaterial, digest_nix_flags, git_source_identity, lookup_store_exe,
    record_store_exe_hit, record_store_exe_miss, store_exe_cache_enabled,
    store_exe_cache_key_digest, store_exe_path_usable, store_store_exe,
};
use nxr_nix::{
    OptionalNixFlags, batched_store_queries_enabled_for_nix, detect_system,
    realise_flake_app_program, store_exe_paths_usable,
};
use nxr_watch::{PrewarmStoreExe, WatchPrewarm};

/// Program + argv chosen for spawn (nix run plan or direct store exe).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedAppSpawn {
    pub program: Utf8PathBuf,
    pub arguments: Vec<String>,
    /// True when the store-exe cache supplied a direct `/nix/store` program.
    pub used_store_exe: bool,
}

/// Resolve an app spawn, preferring a warm realised store executable when safe.
///
/// Falls back to the prepared plan's `nix run` argv on any doubt (disabled cache,
/// shell wrap, missing fingerprints, remote flake, realise failure, invalid path).
#[must_use]
pub fn resolve_app_spawn(
    plan: &Plan,
    nix: &Utf8Path,
    local_root: Option<&Utf8Path>,
    nix_flags: &OptionalNixFlags,
    nix_version: &str,
    cwd: Option<&Path>,
) -> ResolvedAppSpawn {
    resolve_app_spawn_with_prewarm(plan, nix, local_root, nix_flags, nix_version, cwd, None)
}

/// Like [`resolve_app_spawn`], consulting an optional watch-session prewarm cache.
#[must_use]
pub fn resolve_app_spawn_with_prewarm(
    plan: &Plan,
    nix: &Utf8Path,
    local_root: Option<&Utf8Path>,
    nix_flags: &OptionalNixFlags,
    nix_version: &str,
    cwd: Option<&Path>,
    mut prewarm: Option<&mut WatchPrewarm>,
) -> ResolvedAppSpawn {
    let fallback = ResolvedAppSpawn {
        program: Utf8PathBuf::from(plan.command.program.clone()),
        arguments: plan.command.arguments.clone(),
        used_store_exe: false,
    };

    if !store_exe_cache_enabled() {
        return fallback;
    }
    if !plan_eligible_for_store_exe(plan) {
        return fallback;
    }
    let Some(local_root) = local_root else {
        return fallback;
    };
    let Some(fingerprints) = shared_fingerprints(local_root, nix.as_str(), nix_version) else {
        return fallback;
    };

    let key = StoreExeCacheKeyMaterial {
        flake_ref: plan.flake.clone(),
        local_root: local_root.as_str().to_owned(),
        system: plan.system.clone(),
        app_name: plan.target.clone(),
        attr_path: plan.attr_path.clone(),
        nix_flags_digest: digest_nix_flags(
            nix_flags.offline,
            nix_flags.no_write_lock_file,
            nix_flags.accept_flake_config,
            nix_flags.json_log_format,
            &nix_flags.nix_options,
            &nix_flags.extra_argv,
        ),
        fingerprints: fingerprints.clone(),
    };
    let key_digest = store_exe_cache_key_digest(&key);
    let batched = batched_store_queries_enabled_for_nix(nix);

    if let Some(prewarm) = prewarm.as_mut()
        && let Some(hit) = prewarm.lookup_store_exe(&key_digest)
        && store_exe_path_usable(hit.program.as_std_path())
    {
        let resolved = ResolvedAppSpawn {
            program: hit.program.clone(),
            arguments: hit.arguments.clone(),
            used_store_exe: true,
        };
        prewarm.record_store_exe_hit();
        return resolved;
    }

    if let Some(hit) = lookup_store_exe(&key_digest, &fingerprints) {
        let program = Utf8PathBuf::from(hit.program);
        if store_exe_paths_usable(
            nix,
            program.as_std_path(),
            Some(hit.store_output.as_str()),
            batched,
            cwd,
        ) {
            record_store_exe_hit();
            let resolved = ResolvedAppSpawn {
                program: program.clone(),
                arguments: plan.forwarded_arguments.clone(),
                used_store_exe: true,
            };
            if let Some(prewarm) = prewarm {
                prewarm.store_store_exe(PrewarmStoreExe {
                    key_digest,
                    program,
                    arguments: resolved.arguments.clone(),
                });
            }
            return resolved;
        }
    }

    if let Some(prewarm) = prewarm.as_mut() {
        prewarm.record_store_exe_miss();
    }
    record_store_exe_miss();

    let realise_system = match resolve_realise_system(plan, nix) {
        Some(system) => system,
        None => return fallback,
    };

    match realise_flake_app_program(
        nix,
        &plan.flake,
        &realise_system,
        &plan.target,
        nix_flags,
        cwd,
    ) {
        Ok(realised) => {
            let _ = store_store_exe(
                &key_digest,
                realised.program.as_str(),
                realised.store_output.as_str(),
                fingerprints,
            );
            if store_exe_paths_usable(
                nix,
                realised.program.as_std_path(),
                Some(realised.store_output.as_str()),
                batched,
                cwd,
            ) {
                let resolved = ResolvedAppSpawn {
                    program: realised.program.clone(),
                    arguments: plan.forwarded_arguments.clone(),
                    used_store_exe: true,
                };
                if let Some(prewarm) = prewarm {
                    prewarm.store_store_exe(PrewarmStoreExe {
                        key_digest,
                        program: realised.program,
                        arguments: resolved.arguments.clone(),
                    });
                }
                return resolved;
            }
            fallback
        }
        Err(_) => fallback,
    }
}

fn plan_eligible_for_store_exe(plan: &Plan) -> bool {
    if plan.shell.is_some() {
        return false;
    }
    if plan.command.arguments.iter().any(|arg| arg == "develop") {
        return false;
    }
    if !plan.command.arguments.iter().any(|arg| arg == "run") {
        return false;
    }
    if plan_contains_secret_values(&plan.secrets) {
        return false;
    }
    true
}

fn plan_contains_secret_values(secrets: &[PlanSecretRef]) -> bool {
    secrets.iter().any(|secret| {
        let trimmed = secret.value.trim();
        !trimmed.is_empty() && trimmed != PLAN_SECRET_RUNTIME_PLACEHOLDER
    })
}

fn resolve_realise_system(plan: &Plan, nix: &Utf8Path) -> Option<String> {
    if plan.system != "local" && !plan.system.is_empty() {
        return Some(plan.system.clone());
    }
    detect_system(nix).ok()
}

pub(crate) fn shared_fingerprints(
    local_root: &Utf8Path,
    nix_path: &str,
    nix_version: &str,
) -> Option<PlanCacheSharedFingerprints> {
    let nix_tree = nix_tree_fingerprint(local_root).ok()?;
    let discovery_inputs = hint_discovery_inputs_for_root(local_root);
    let discovery_fp = discovery_inputs_fingerprint(local_root, &discovery_inputs).ok()?;
    let git = git_source_identity(local_root).ok().flatten();
    // Refuse reuse when we lack both git identity and declared discovery inputs,
    // or when the tree is dirty with no discoveryInputs (gitignore / untracked
    // package sources would otherwise stay invisible).
    let source_identity = match &git {
        Some(identity) if identity.dirty && discovery_inputs.is_empty() => return None,
        Some(identity) => Some(identity.digest.clone()),
        None if discovery_inputs.is_empty() => return None,
        None => None,
    };
    let flake_lock = flake_lock_digest(local_root).ok().flatten();
    let nix_file_identity = nix_executable_identity(nix_path);
    Some(PlanCacheSharedFingerprints {
        nix_tree_fingerprint: nix_tree,
        discovery_inputs_fingerprint: discovery_fp,
        flake_lock_digest: flake_lock,
        nix_path: nix_path.to_owned(),
        nix_version: nix_version.to_owned(),
        nix_file_identity,
        source_identity,
    })
}

fn nix_executable_identity(nix_path: &str) -> Option<String> {
    let path = Utf8Path::new(nix_path);
    let canonical = path
        .canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf());
    let metadata = std::fs::metadata(canonical.as_std_path()).ok()?;
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
    use super::plan_eligible_for_store_exe;
    use nxr_core::{EnvironmentPolicy, Plan, PlanCommand, PlanKind, PlanSecretRef};
    use std::collections::BTreeMap;

    fn base_plan() -> Plan {
        Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: "/proj".to_owned(),
            system: "local".to_owned(),
            target: "hello".to_owned(),
            attr_path: "apps.local.hello".to_owned(),
            invocation_directory: "/proj".to_owned(),
            execution_directory: "/proj".to_owned(),
            shell: None,
            active_shell: None,
            environment_policy: EnvironmentPolicy::Inherit,
            context: None,
            secrets: Vec::new(),
            context_env_set: BTreeMap::new(),
            parameters: Vec::new(),
            matrix: Vec::new(),
            command: PlanCommand {
                program: "/nix/bin/nix".to_owned(),
                arguments: vec!["run".to_owned(), "/proj#hello".to_owned()],
            },
            forwarded_arguments: Vec::new(),
            workspace_script: None,
            mutable_source: false,
            fallback_app: None,
            environment_mode: None,
        }
    }

    #[test]
    fn eligible_for_plain_nix_run_plans() {
        assert!(plan_eligible_for_store_exe(&base_plan()));
    }

    #[test]
    fn rejects_develop_wrap_and_secret_values() {
        let mut plan = base_plan();
        plan.command.arguments = vec![
            "develop".to_owned(),
            "/proj#dev".to_owned(),
            "-c".to_owned(),
            "/nix/bin/nix".to_owned(),
            "run".to_owned(),
            "/proj#hello".to_owned(),
        ];
        assert!(!plan_eligible_for_store_exe(&plan));

        let mut plan = base_plan();
        plan.shell = Some("dev".to_owned());
        assert!(!plan_eligible_for_store_exe(&plan));

        let mut plan = base_plan();
        plan.secrets.push(PlanSecretRef {
            name: "t".to_owned(),
            reference: "T".to_owned(),
            delivery: "env".to_owned(),
            provider: "env".to_owned(),
            value: "secret".to_owned(),
        });
        assert!(!plan_eligible_for_store_exe(&plan));
    }
}
