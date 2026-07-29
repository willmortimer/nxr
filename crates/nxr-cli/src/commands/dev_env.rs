//! Materialized dev-environment resolution for workspace scripts (ADR-0171).

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::{
    DEV_ENV_PROTOCOL_VERSION, DaemonDevEnvEntry, DevEnvironmentCacheKeyMaterial,
    DevEnvironmentNixIdentity, DevEnvironmentSnapshot, EnvironmentPolicy,
    PlanCacheSharedFingerprints, daemon_dev_env_entry, dev_env_cache_key_digest, digest_nix_flags,
    lookup_dev_environment_snapshot, record_dev_env_cache_hit, record_dev_env_cache_miss,
    store_dev_environment_snapshot, try_once,
};
use nxr_nix::{
    DevEnvironment, NixError, NixFailureKind, OptionalNixFlags, detect_capabilities, detect_system,
    nix_develop_wrap_command_args, parse_print_dev_env_json, print_dev_env_args, run_nix,
    unsupported_feature_label,
};

use crate::commands::script::ScriptSpawn;
use crate::commands::store_exe::shared_fingerprints;
use crate::flake::FlakeSelection;
use crate::shell_mode::{ShellMode, effective_shell_wrap};

/// Plan field value: direct spawn with a materialized process environment.
pub const ENV_MODE_PROCESS: &str = "process";
/// Plan field value: `nix develop -c` shell wrap.
pub const ENV_MODE_SHELL: &str = "shell";

/// Result of resolving how to spawn a workspace script with an optional dev shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedScriptSpawn {
    pub program: Utf8PathBuf,
    pub arguments: Vec<String>,
    pub environment_policy: EnvironmentPolicy,
    pub environment_mode: Option<String>,
}

/// Apply ADR-0171 spawn hierarchy for workspace scripts / live file-backed apps.
///
/// # Errors
///
/// Returns [`NixError`] when Nix capability detection or `print-dev-env` fails.
pub fn resolve_script_spawn_with_dev_env(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    local_root: &Utf8Path,
    shell: Option<&str>,
    shell_mode: ShellMode,
    base_environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
    spawn: &ScriptSpawn,
    forwarded: &[String],
) -> Result<ResolvedScriptSpawn, NixError> {
    let mut inner_args = spawn.prefix_args.clone();
    inner_args.extend(forwarded.iter().cloned());

    let direct = || ResolvedScriptSpawn {
        program: spawn.program.clone(),
        arguments: inner_args.clone(),
        environment_policy: base_environment_policy.clone(),
        environment_mode: None,
    };

    let Some(shell_name) = shell else {
        return Ok(direct());
    };

    if matches!(shell_mode, ShellMode::Never) {
        return Ok(direct());
    }

    if effective_shell_wrap(Some(shell_name), shell_mode).is_none() {
        // Active `NXR_DEV_SHELL` matches — inherit caller environment (0 Nix).
        return Ok(direct());
    }

    if matches!(shell_mode, ShellMode::Always) {
        return Ok(develop_wrap(
            flake,
            nix,
            shell_name,
            nix_flags,
            spawn,
            &inner_args,
            base_environment_policy,
        )?);
    }

    match try_process_spawn(
        flake,
        nix,
        local_root,
        shell_name,
        base_environment_policy,
        nix_flags,
        spawn,
        &inner_args,
    ) {
        Ok(Some(resolved)) => Ok(resolved),
        Ok(None) | Err(_) => Ok(develop_wrap(
            flake,
            nix,
            shell_name,
            nix_flags,
            spawn,
            &inner_args,
            base_environment_policy,
        )?),
    }
}

fn develop_wrap(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    shell_name: &str,
    nix_flags: &OptionalNixFlags,
    spawn: &ScriptSpawn,
    inner_args: &[String],
    base_environment_policy: &EnvironmentPolicy,
) -> Result<ResolvedScriptSpawn, NixError> {
    let develop_argv = nix_develop_wrap_command_args(
        &flake.nix_ref,
        shell_name,
        spawn.program.as_str(),
        inner_args,
    );
    let capabilities = detect_capabilities(nix)?;
    let arguments = capabilities.apply_optional_flags(develop_argv, nix_flags)?;
    Ok(ResolvedScriptSpawn {
        program: nix.to_path_buf(),
        arguments,
        environment_policy: base_environment_policy.clone(),
        environment_mode: Some(ENV_MODE_SHELL.to_owned()),
    })
}

fn try_process_spawn(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    local_root: &Utf8Path,
    shell_name: &str,
    base_environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
    spawn: &ScriptSpawn,
    inner_args: &[String],
) -> Result<Option<ResolvedScriptSpawn>, NixError> {
    let capabilities = detect_capabilities(nix)?;
    if !capabilities.supports_print_dev_env_json {
        return Ok(None);
    }

    let nix_version = capabilities.version.to_string();
    let Some(fingerprints) = shared_fingerprints(local_root, nix.as_str(), &nix_version) else {
        return Ok(None);
    };

    let system = detect_system(nix)?;
    let nix_flags_digest = digest_nix_flags(
        nix_flags.offline,
        nix_flags.no_write_lock_file,
        nix_flags.accept_flake_config,
        nix_flags.json_log_format,
        &nix_flags.nix_options,
        &nix_flags.extra_argv,
    );
    let key_material = DevEnvironmentCacheKeyMaterial {
        flake_identity: flake.nix_ref.clone(),
        local_root: local_root.as_str().to_owned(),
        system: system.clone(),
        shell_name: shell_name.to_owned(),
        environment_mode: ENV_MODE_PROCESS.to_owned(),
        nix_flags_digest,
        fingerprints: fingerprints.clone(),
        protocol_version: DEV_ENV_PROTOCOL_VERSION,
    };
    let key_digest = dev_env_cache_key_digest(&key_material);

    let snapshot = if let Some(snapshot) = lookup_dev_env_snapshot(&key_digest, &fingerprints) {
        snapshot
    } else {
        record_dev_env_cache_miss();
        let parsed = match fetch_print_dev_env(nix, &flake.nix_ref, shell_name, nix_flags) {
            Some(parsed) => parsed,
            None => return Ok(None),
        };
        if !parsed.is_process_compatible() {
            return Ok(None);
        }
        let snapshot = dev_environment_to_snapshot(
            flake,
            &system,
            shell_name,
            nix,
            &nix_version,
            &fingerprints,
            &parsed,
        );
        let _ = store_dev_environment_snapshot(&key_digest, &snapshot, fingerprints.clone());
        try_daemon_dev_env_store(&key_digest, &snapshot, fingerprints.clone());
        snapshot
    };

    Ok(Some(ResolvedScriptSpawn {
        program: spawn.program.clone(),
        arguments: inner_args.to_vec(),
        environment_policy: environment_policy_with_snapshot(base_environment_policy, &snapshot),
        environment_mode: Some(ENV_MODE_PROCESS.to_owned()),
    }))
}

fn fetch_print_dev_env(
    nix: &Utf8Path,
    flake_ref: &str,
    shell_name: &str,
    nix_flags: &OptionalNixFlags,
) -> Option<DevEnvironment> {
    let base_argv = print_dev_env_args(flake_ref, shell_name);
    let capabilities = detect_capabilities(nix).ok()?;
    let argv = capabilities
        .apply_optional_flags(base_argv, nix_flags)
        .ok()?;
    let stdout = run_nix(nix, &argv, NixFailureKind::Evaluation).ok()?;
    let json = String::from_utf8(stdout).ok()?;
    parse_print_dev_env_json(&json).ok()
}

fn dev_environment_to_snapshot(
    flake: &FlakeSelection,
    system: &str,
    shell_name: &str,
    nix: &Utf8Path,
    nix_version: &str,
    fingerprints: &PlanCacheSharedFingerprints,
    env: &DevEnvironment,
) -> DevEnvironmentSnapshot {
    DevEnvironmentSnapshot {
        flake_identity: flake.nix_ref.clone(),
        system: system.to_owned(),
        shell: shell_name.to_owned(),
        nix_identity: DevEnvironmentNixIdentity {
            nix_path: nix.as_str().to_owned(),
            nix_version: nix_version.to_owned(),
            nix_file_identity: fingerprints.nix_file_identity.clone(),
            nix_tree_fingerprint: fingerprints.nix_tree_fingerprint.clone(),
        },
        variables: env.variables.clone(),
        path_entries: env.path_entries.clone(),
        unsupported_features: env
            .unsupported_features
            .iter()
            .map(unsupported_feature_label)
            .map(str::to_owned)
            .collect(),
        fingerprints: fingerprints.clone(),
        protocol_version: DEV_ENV_PROTOCOL_VERSION,
        secret_variables: Vec::new(),
    }
}

fn environment_policy_with_snapshot(
    base: &EnvironmentPolicy,
    snapshot: &DevEnvironmentSnapshot,
) -> EnvironmentPolicy {
    let mut set = base_set_map(base);
    for (name, value) in &snapshot.variables {
        set.insert(name.clone(), value.clone());
    }
    if !snapshot.path_entries.is_empty() {
        set.insert("PATH".to_owned(), snapshot.path_entries.join(":"));
    }
    match base {
        EnvironmentPolicy::Inherit => EnvironmentPolicy::inherit_with([], set, []),
        EnvironmentPolicy::InheritWith { keep, unset, .. } => {
            EnvironmentPolicy::inherit_with(keep.clone(), set, unset.clone())
        }
        EnvironmentPolicy::Clean { keep, unset, .. } => {
            EnvironmentPolicy::clean(keep.clone(), set, unset.clone())
        }
    }
}

fn base_set_map(policy: &EnvironmentPolicy) -> BTreeMap<String, String> {
    match policy {
        EnvironmentPolicy::Inherit => BTreeMap::new(),
        EnvironmentPolicy::InheritWith { set, .. } | EnvironmentPolicy::Clean { set, .. } => {
            set.clone()
        }
    }
}

fn lookup_dev_env_snapshot(
    key_digest: &str,
    fingerprints: &PlanCacheSharedFingerprints,
) -> Option<DevEnvironmentSnapshot> {
    if let Some(snapshot) = try_daemon_dev_env_lookup(key_digest, fingerprints) {
        return Some(snapshot);
    }
    if let Some(hit) = lookup_dev_environment_snapshot(key_digest, fingerprints) {
        try_daemon_dev_env_store(key_digest, &hit.snapshot, fingerprints.clone());
        return Some(hit.snapshot);
    }
    None
}

fn try_daemon_dev_env_lookup(
    key_digest: &str,
    expected: &PlanCacheSharedFingerprints,
) -> Option<DevEnvironmentSnapshot> {
    let result: serde_json::Value = try_once(
        "dev_env.get",
        Some(serde_json::json!({ "key_digest": key_digest })),
    )?;
    if result.get("hit").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let entry: DaemonDevEnvEntry = serde_json::from_value(result.get("entry")?.clone()).ok()?;
    if &entry.fingerprints != expected {
        return None;
    }
    record_dev_env_cache_hit();
    Some(entry.snapshot)
}

fn try_daemon_dev_env_store(
    key_digest: &str,
    snapshot: &DevEnvironmentSnapshot,
    fingerprints: PlanCacheSharedFingerprints,
) {
    let Some(entry) = daemon_dev_env_entry(snapshot, fingerprints) else {
        return;
    };
    let _: Option<serde_json::Value> = try_once(
        "dev_env.put",
        Some(serde_json::json!({
            "key_digest": key_digest,
            "entry": entry,
        })),
    );
}

/// Materialize a process-compatible shell policy for one-shell DAG runs (ADR-0129).
///
/// # Errors
///
/// Returns [`NixError`] when Nix subprocesses fail.
pub(crate) fn materialize_process_shell_policy(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    local_root: &Utf8Path,
    shell_name: &str,
    base_environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
) -> Result<Option<EnvironmentPolicy>, NixError> {
    try_one_shell_process_policy(
        flake,
        nix,
        local_root,
        shell_name,
        base_environment_policy,
        nix_flags,
    )
}

/// Capture a development shell via `nix develop -c env` for one-shell DAG runs.
///
/// # Errors
///
/// Returns [`NixError`] when Nix subprocesses fail.
pub(crate) fn materialize_develop_shell_policy(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    shell_name: &str,
    base_environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
) -> Result<EnvironmentPolicy, NixError> {
    let vars = capture_develop_process_env(flake, nix, shell_name, nix_flags)?;
    Ok(environment_policy_with_vars(base_environment_policy, &vars))
}

fn try_one_shell_process_policy(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    local_root: &Utf8Path,
    shell_name: &str,
    base_environment_policy: &EnvironmentPolicy,
    nix_flags: &OptionalNixFlags,
) -> Result<Option<EnvironmentPolicy>, NixError> {
    let capabilities = detect_capabilities(nix)?;
    if !capabilities.supports_print_dev_env_json {
        return Ok(None);
    }

    let nix_version = capabilities.version.to_string();
    let Some(fingerprints) = shared_fingerprints(local_root, nix.as_str(), &nix_version) else {
        return Ok(None);
    };

    let system = detect_system(nix)?;
    let nix_flags_digest = digest_nix_flags(
        nix_flags.offline,
        nix_flags.no_write_lock_file,
        nix_flags.accept_flake_config,
        nix_flags.json_log_format,
        &nix_flags.nix_options,
        &nix_flags.extra_argv,
    );
    let key_material = DevEnvironmentCacheKeyMaterial {
        flake_identity: flake.nix_ref.clone(),
        local_root: local_root.as_str().to_owned(),
        system: system.clone(),
        shell_name: shell_name.to_owned(),
        environment_mode: ENV_MODE_PROCESS.to_owned(),
        nix_flags_digest,
        fingerprints: fingerprints.clone(),
        protocol_version: DEV_ENV_PROTOCOL_VERSION,
    };
    let key_digest = dev_env_cache_key_digest(&key_material);

    let snapshot = if let Some(snapshot) = lookup_dev_env_snapshot(&key_digest, &fingerprints) {
        snapshot
    } else {
        record_dev_env_cache_miss();
        let parsed = match fetch_print_dev_env(nix, &flake.nix_ref, shell_name, nix_flags) {
            Some(parsed) => parsed,
            None => return Ok(None),
        };
        if !parsed.is_process_compatible() {
            return Ok(None);
        }
        let snapshot = dev_environment_to_snapshot(
            flake,
            &system,
            shell_name,
            nix,
            &nix_version,
            &fingerprints,
            &parsed,
        );
        let _ = store_dev_environment_snapshot(&key_digest, &snapshot, fingerprints.clone());
        try_daemon_dev_env_store(&key_digest, &snapshot, fingerprints.clone());
        snapshot
    };

    Ok(Some(environment_policy_with_snapshot(
        base_environment_policy,
        &snapshot,
    )))
}

fn capture_develop_process_env(
    flake: &FlakeSelection,
    nix: &Utf8Path,
    shell_name: &str,
    nix_flags: &OptionalNixFlags,
) -> Result<BTreeMap<String, String>, NixError> {
    let base_argv =
        nix_develop_wrap_command_args(&flake.nix_ref, shell_name, "env", &[] as &[&str]);
    let capabilities = detect_capabilities(nix)?;
    let argv = capabilities.apply_optional_flags(base_argv, nix_flags)?;
    let stdout = run_nix(nix, &argv, NixFailureKind::Evaluation)?;
    parse_env_stdout(&stdout)
}

fn parse_env_stdout(stdout: &[u8]) -> Result<BTreeMap<String, String>, NixError> {
    let text = String::from_utf8_lossy(stdout);
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if !key.is_empty() {
                map.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    Ok(map)
}

fn environment_policy_with_vars(
    base: &EnvironmentPolicy,
    vars: &BTreeMap<String, String>,
) -> EnvironmentPolicy {
    let mut set = base_set_map(base);
    set.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    match base {
        EnvironmentPolicy::Inherit => EnvironmentPolicy::inherit_with([], set, []),
        EnvironmentPolicy::InheritWith { keep, unset, .. } => {
            EnvironmentPolicy::inherit_with(keep.clone(), set, unset.clone())
        }
        EnvironmentPolicy::Clean { keep, unset, .. } => {
            EnvironmentPolicy::clean(keep.clone(), set, unset.clone())
        }
    }
}
