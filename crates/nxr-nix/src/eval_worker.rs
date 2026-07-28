//! Experimental opt-in eval worker client ([ADR-0168]).
//!
//! When `NXR_EVAL_WORKER=1` and the host is `eval_worker_eligible`, try
//! `nxrd` `eval.get` / `eval.put` for narrowly typed JSON. Any doubt falls
//! back to subprocess [`crate::capabilities::run_nix`].

use std::path::Path;

use blake3::Hasher;
use camino::{Utf8Path, Utf8PathBuf};
use nxr_core::{EvalKind, EvalPrepareParams, eval_worker_enabled, try_once};
use serde_json::Value;

use crate::NixError;
use crate::capabilities::{NixFailureKind, run_nix};
use crate::strategy::plan_discovery_eval;

/// Context required to prepare / key an eval-worker session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalWorkerContext {
    pub nix_path: String,
    pub nix_version_banner: String,
    pub config_json: Option<String>,
    pub flake_root: String,
    pub flake_fingerprint: String,
    /// When false (non-Determinate), the worker is never attempted.
    pub eligible: bool,
}

impl EvalWorkerContext {
    /// Build prepare params for the daemon session.
    #[must_use]
    pub fn prepare_params(&self) -> EvalPrepareParams {
        EvalPrepareParams {
            nix_identity: nix_identity(&self.nix_path, &self.nix_version_banner),
            config_fingerprint: self.config_json.as_deref().map(fingerprint_bytes),
            flake_root: self.flake_root.clone(),
            flake_fingerprint: self.flake_fingerprint.clone(),
        }
    }

    /// Whether this context may attempt the worker (opt-in + eligibility).
    #[must_use]
    pub fn may_use_worker(&self) -> bool {
        self.may_use_worker_with(eval_worker_enabled())
    }

    /// Test/helper variant with an explicit opt-in flag.
    #[must_use]
    pub fn may_use_worker_with(&self, opted_in: bool) -> bool {
        opted_in && self.eligible
    }
}

/// Build an eval-worker context from Nix identity + local flake root when possible.
#[must_use]
pub fn eval_worker_context_for(
    nix_path: &str,
    version_banner: &str,
    config_json: Option<&str>,
    flake_root: &Utf8Path,
) -> Option<EvalWorkerContext> {
    let flake_fingerprint = flake_inputs_fingerprint(flake_root.as_std_path())?;
    let plan = plan_discovery_eval(version_banner, config_json, false);
    Some(EvalWorkerContext {
        nix_path: nix_path.to_owned(),
        nix_version_banner: version_banner.to_owned(),
        config_json: config_json.map(str::to_owned),
        flake_root: flake_root.as_str().to_owned(),
        flake_fingerprint,
        eligible: plan.eval_worker_eligible,
    })
}

/// Fingerprint opaque bytes (config JSON, file digests, …).
#[must_use]
pub fn fingerprint_bytes(input: impl AsRef<[u8]>) -> String {
    let mut hasher = Hasher::new();
    hasher.update(input.as_ref());
    hasher.finalize().to_hex().to_string()
}

/// Compose a stable nix identity string (path + banner).
#[must_use]
pub fn nix_identity(nix_path: &str, version_banner: &str) -> String {
    format!(
        "{}|{}",
        nix_path.trim(),
        fingerprint_bytes(version_banner.trim().as_bytes())
    )
}

/// Build a cache key for one eval kind + attr path under a flake fingerprint.
#[must_use]
pub fn eval_cache_key(kind: EvalKind, flake_fingerprint: &str, attr_or_label: &str) -> String {
    format!(
        "{}:{}:{}",
        kind.as_str(),
        flake_fingerprint,
        attr_or_label.trim()
    )
}

/// Best-effort fingerprint of flake.nix + flake.lock under `flake_root`.
///
/// Uses path metadata (size + mtime nanos) so callers avoid hashing large trees.
/// Returns `None` when neither file is readable — callers should skip the worker.
#[must_use]
pub fn flake_inputs_fingerprint(flake_root: &Path) -> Option<String> {
    let mut hasher = Hasher::new();
    let mut saw_any = false;
    for name in ["flake.nix", "flake.lock"] {
        let path = flake_root.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        saw_any = true;
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hasher.update(&meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hasher.update(&duration.as_nanos().to_le_bytes());
        }
    }
    if !saw_any {
        return None;
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// Run `nix eval`-shaped argv, preferring the experimental worker cache when enabled.
///
/// # Errors
///
/// Returns [`NixError`] from the subprocess path when the worker misses or is unavailable.
pub fn eval_json_with_worker(
    nix: &Utf8Path,
    args: &[String],
    kind: EvalKind,
    attr_or_label: &str,
    context: Option<&EvalWorkerContext>,
) -> Result<Vec<u8>, NixError> {
    eval_json_with_worker_opt_in(
        nix,
        args,
        kind,
        attr_or_label,
        context,
        eval_worker_enabled(),
    )
}

fn eval_json_with_worker_opt_in(
    nix: &Utf8Path,
    args: &[String],
    kind: EvalKind,
    attr_or_label: &str,
    context: Option<&EvalWorkerContext>,
    opted_in: bool,
) -> Result<Vec<u8>, NixError> {
    if let Some(context) = context
        && context.may_use_worker_with(opted_in)
        && let Some(bytes) = try_worker_get_if(context, kind, attr_or_label, true)
    {
        return Ok(bytes);
    }

    let stdout = run_nix(nix, args, NixFailureKind::Evaluation)?;

    if let Some(context) = context
        && context.may_use_worker_with(opted_in)
    {
        let _ = try_worker_put_if(context, kind, attr_or_label, &stdout, true);
    }

    Ok(stdout)
}

/// Absent / disabled / ineligible worker behaves as a transparent no-op.
#[must_use]
pub fn try_worker_get(
    context: &EvalWorkerContext,
    kind: EvalKind,
    attr_or_label: &str,
) -> Option<Vec<u8>> {
    try_worker_get_if(context, kind, attr_or_label, eval_worker_enabled())
}

fn try_worker_get_if(
    context: &EvalWorkerContext,
    kind: EvalKind,
    attr_or_label: &str,
    opted_in: bool,
) -> Option<Vec<u8>> {
    if !context.may_use_worker_with(opted_in) {
        return None;
    }
    let session = context.prepare_params();
    let _: Value = try_once("eval.prepare", Some(serde_json::to_value(&session).ok()?))?;
    let cache_key = eval_cache_key(kind, &context.flake_fingerprint, attr_or_label);
    let result: Value = try_once(
        "eval.get",
        Some(serde_json::json!({
            "session": session,
            "kind": kind.as_str(),
            "cache_key": cache_key,
        })),
    )?;
    if result.get("hit").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let json = result.get("json")?;
    serde_json::to_vec(json).ok()
}

fn try_worker_put_if(
    context: &EvalWorkerContext,
    kind: EvalKind,
    attr_or_label: &str,
    stdout: &[u8],
    opted_in: bool,
) -> Option<()> {
    if !context.may_use_worker_with(opted_in) {
        return None;
    }
    let json: Value = serde_json::from_slice(stdout).ok()?;
    let session = context.prepare_params();
    let _: Value = try_once("eval.prepare", Some(serde_json::to_value(&session).ok()?))?;
    let cache_key = eval_cache_key(kind, &context.flake_fingerprint, attr_or_label);
    let _: Value = try_once(
        "eval.put",
        Some(serde_json::json!({
            "session": session,
            "kind": kind.as_str(),
            "cache_key": cache_key,
            "json": json,
        })),
    )?;
    Some(())
}

/// Resolve a local flake root from a `path:` flake ref when present.
#[must_use]
pub fn local_root_from_flake_ref(flake_ref: &str) -> Option<Utf8PathBuf> {
    let path = flake_ref.strip_prefix("path:")?;
    let utf8 = Utf8PathBuf::from(path);
    if utf8.as_str().is_empty() {
        return None;
    }
    Some(utf8)
}

#[cfg(test)]
mod tests {
    use super::{
        EvalWorkerContext, eval_cache_key, eval_json_with_worker_opt_in, flake_inputs_fingerprint,
        local_root_from_flake_ref, nix_identity, try_worker_get, try_worker_get_if,
    };
    use crate::NixError;
    use crate::capabilities::NixFailureKind;
    use camino::{Utf8Path, Utf8PathBuf};
    use nxr_core::{DAEMON_PROTOCOL_VERSION, DaemonState, EvalKind, serve, try_connect};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn cache_key_stable() {
        assert_eq!(
            eval_cache_key(EvalKind::Metadata, "fp", "nxrMetadata.x86_64-linux"),
            "metadata:fp:nxrMetadata.x86_64-linux"
        );
    }

    #[test]
    fn nix_identity_includes_path() {
        let id = nix_identity("/bin/nix", "nix (Determinate Nix 3.0) 2.24\n");
        assert!(id.starts_with("/bin/nix|"));
    }

    #[test]
    fn local_root_from_path_ref() {
        assert_eq!(
            local_root_from_flake_ref("path:/tmp/flake")
                .as_deref()
                .map(Utf8Path::as_str),
            Some("/tmp/flake")
        );
        assert!(local_root_from_flake_ref("github:owner/repo").is_none());
    }

    #[test]
    fn absent_worker_get_is_none_when_disabled() {
        let ctx = EvalWorkerContext {
            nix_path: "/bin/nix".to_owned(),
            nix_version_banner: "nix".to_owned(),
            config_json: None,
            flake_root: "/repo".to_owned(),
            flake_fingerprint: "fp".to_owned(),
            eligible: true,
        };
        assert!(try_worker_get_if(&ctx, EvalKind::Tasks, "nxr.x86_64-linux", false).is_none());
        // Default path reads env; without opt-in this is also a miss.
        assert!(try_worker_get(&ctx, EvalKind::Tasks, "nxr.x86_64-linux").is_none());
    }

    #[test]
    fn ineligible_skips_even_when_opt_in() {
        let ctx = EvalWorkerContext {
            nix_path: "/bin/nix".to_owned(),
            nix_version_banner: "nix".to_owned(),
            config_json: None,
            flake_root: "/repo".to_owned(),
            flake_fingerprint: "fp".to_owned(),
            eligible: false,
        };
        assert!(!ctx.may_use_worker_with(true));
        assert!(try_worker_get_if(&ctx, EvalKind::List, "show", true).is_none());
    }

    #[test]
    fn flake_inputs_fingerprint_reads_fixture_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flake.nix"), "{ }").unwrap();
        std::fs::write(dir.path().join("flake.lock"), "{}").unwrap();
        let fp = flake_inputs_fingerprint(dir.path()).expect("fp");
        assert_eq!(fp.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn worker_round_trip_then_fallback_path() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nxrd.sock");
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let serve_state = Arc::clone(&state);
        let serve_socket = socket.clone();
        let server = thread::spawn(move || serve(&serve_socket, serve_state));

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(socket.exists());

        let ctx = EvalWorkerContext {
            nix_path: "/bin/nix".to_owned(),
            nix_version_banner: "nix (Determinate Nix 3.0) 2.24\n".to_owned(),
            config_json: Some(r#"{"lazy-trees":{"value":true}}"#.to_owned()),
            flake_root: "/repo".to_owned(),
            flake_fingerprint: "fp-round".to_owned(),
            eligible: true,
        };

        let session = ctx.prepare_params();
        let mut conn = try_connect(&socket).expect("connect");
        let _: serde_json::Value = conn
            .call(
                "eval.prepare",
                Some(serde_json::to_value(&session).unwrap()),
            )
            .expect("prepare");
        let _: serde_json::Value = conn
            .call(
                "eval.put",
                Some(json!({
                    "session": session,
                    "kind": "metadata",
                    "cache_key": eval_cache_key(EvalKind::Metadata, "fp-round", "nxrMetadata.test"),
                    "json": { "schema_version": 1, "inventory": { "apps": ["hello"] } },
                })),
            )
            .expect("put");

        let get: serde_json::Value = conn
            .call(
                "eval.get",
                Some(json!({
                    "session": ctx.prepare_params(),
                    "kind": "metadata",
                    "cache_key": eval_cache_key(EvalKind::Metadata, "fp-round", "nxrMetadata.test"),
                })),
            )
            .expect("get");
        assert_eq!(get["hit"], true);
        assert_eq!(get["json"]["schema_version"], 1);

        let nix = Utf8PathBuf::from("/usr/bin/false");
        let err = eval_json_with_worker_opt_in(
            &nix,
            &["eval".into(), "--json".into(), ".#missing".into()],
            EvalKind::Metadata,
            "nxrMetadata.test",
            Some(&ctx),
            false,
        )
        .expect_err("subprocess fallback");
        assert!(matches!(
            err,
            NixError::CommandFailed {
                kind: NixFailureKind::Evaluation,
                ..
            } | NixError::SpawnFailed { .. }
        ));

        let mut shutdown = try_connect(&socket).expect("shutdown");
        let _: serde_json::Value = shutdown.call("shutdown", None).expect("shutdown");
        let _ = server.join();
        let _ = DAEMON_PROTOCOL_VERSION;
    }
}
