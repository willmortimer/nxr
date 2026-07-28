//! Optional local cache/coordination daemon (`nxrd` / `nxr daemon`).
//!
//! Retains warm discovery, prepared-plan, fingerprint, Merkle invalidation,
//! recent action-key material, an optional process-log broker, and an
//! experimental eval-result worker across CLI invocations over a per-user Unix
//! socket. The daemon is **not** an execution authority and is never required
//! for correctness
//! ([ADR-0157](../../../docs/adr/0157-optional-nxrd.md);
//! [ADR-0164](../../../docs/adr/0164-process-log-broker.md);
//! [ADR-0168](../../../docs/adr/0168-experimental-eval-worker.md); ADR-0301 spirit).
//! Kill-switch: [`DAEMON_ENV`]=`off`. Log broker follow kill-switch:
//! [`crate::log_broker::LOG_BROKER_ENV`]. Eval worker opt-in:
//! [`crate::eval_worker::EVAL_WORKER_ENV`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::eval_worker::{EvalKind, EvalPrepareParams, EvalWorkerCache};
use crate::log_broker::{FILE_POLL_MS, LogBroker, LogEvent, decode_log_bytes, encode_log_bytes};
use crate::merkle_index::touched_directories;
use crate::plan::{Plan, PlanSecretRef};
use crate::plan_cache::{
    PLAN_SECRET_RUNTIME_PLACEHOLDER, PlanCacheSharedFingerprints, PlanPrepareKind,
    PreparedPlanCacheHit, plan_contains_secret_values,
};

/// Protocol version for the Unix-socket JSON-lines API.
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

/// Kill-switch / connect policy (`off` / `0` / `false` / `no` refuse connect).
pub const DAEMON_ENV: &str = "NXR_DAEMON";

/// Override socket path (tests and explicit ops).
pub const DAEMON_SOCKET_ENV: &str = "NXR_DAEMON_SOCKET";

/// Role advertised in hello / status (cache + coordination only).
pub const DAEMON_ROLE: &str = "cache";

/// Maximum entries retained per in-memory map.
const MAX_ENTRIES_PER_MAP: usize = 256;

/// Whether the CLI may attempt to connect to a local daemon.
///
/// Default: enabled. Disabled when `NXR_DAEMON` is `off` / `0` / `false` / `no`.
#[must_use]
pub fn daemon_connect_enabled() -> bool {
    daemon_connect_enabled_for(std::env::var(DAEMON_ENV).ok().as_deref())
}

fn daemon_connect_enabled_for(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "off" | "0" | "false" | "no")
        }
        None => true,
    }
}

/// Resolve the Unix socket path for the local daemon.
///
/// Order: `NXR_DAEMON_SOCKET` → `$XDG_RUNTIME_DIR/nxr/nxrd.sock` →
/// `$TMPDIR/nxr-<user>/nxrd.sock`.
#[must_use]
pub fn daemon_socket_path() -> PathBuf {
    if let Ok(override_path) = std::env::var(DAEMON_SOCKET_ENV)
        && !override_path.trim().is_empty()
    {
        return PathBuf::from(override_path);
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        && !runtime.trim().is_empty()
    {
        return PathBuf::from(runtime).join("nxr").join("nxrd.sock");
    }
    let tmp = std::env::temp_dir();
    let token = std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("UID").ok())
        .unwrap_or_else(|| "user".to_owned());
    tmp.join(format!("nxr-{token}")).join("nxrd.sock")
}

/// Sidecar PID file next to the socket.
#[must_use]
pub fn daemon_pid_path(socket: &Path) -> PathBuf {
    socket.with_extension("pid")
}

/// One JSON-lines request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub v: u32,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// One JSON-lines response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonErrorBody>,
}

/// Error body when `ok` is false.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonErrorBody {
    pub code: String,
    pub message: String,
}

/// Hello / status payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonHello {
    pub protocol_version: u32,
    pub role: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
}

/// Aggregate status for operators / `nxr daemon status`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub protocol_version: u32,
    pub role: String,
    pub pid: u32,
    pub uptime_ms: u64,
    pub socket: String,
    pub discovery_entries: usize,
    pub plan_entries: usize,
    pub fingerprint_entries: usize,
    pub merkle_roots: usize,
    pub action_key_entries: usize,
    pub log_streams: usize,
    pub eval_entries: usize,
}

/// Prepared-plan entry retained in daemon memory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonPlanEntry {
    pub prepare_kind: PlanPrepareKind,
    pub plan: Plan,
    pub nix: String,
    pub execution_directory: String,
    pub fingerprints: PlanCacheSharedFingerprints,
}

/// In-memory daemon state (cache / coordination only).
#[derive(Debug, Default)]
pub struct DaemonState {
    discovery: BTreeMap<String, Value>,
    plans: BTreeMap<String, DaemonPlanEntry>,
    fingerprints: BTreeMap<String, String>,
    /// Flake root → recently invalidated repo-relative paths.
    merkle_invalidated: BTreeMap<String, BTreeSet<String>>,
    /// Recent action-key digests (never secret values).
    action_keys: BTreeMap<String, String>,
    /// Optional process-log broker ([ADR-0164]).
    pub(crate) logs: LogBroker,
    /// Experimental eval JSON worker ([ADR-0168]).
    pub(crate) evals: EvalWorkerCache,
    started_at: Option<Instant>,
    stop: bool,
}

impl DaemonState {
    /// Create a fresh state marked as just started.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started_at: Some(Instant::now()),
            ..Self::default()
        }
    }

    fn uptime_ms(&self) -> u64 {
        self.started_at
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    fn status(&self, socket: &Path) -> DaemonStatus {
        DaemonStatus {
            protocol_version: DAEMON_PROTOCOL_VERSION,
            role: DAEMON_ROLE.to_owned(),
            pid: std::process::id(),
            uptime_ms: self.uptime_ms(),
            socket: socket.display().to_string(),
            discovery_entries: self.discovery.len(),
            plan_entries: self.plans.len(),
            fingerprint_entries: self.fingerprints.len(),
            merkle_roots: self.merkle_invalidated.len(),
            action_key_entries: self.action_keys.len(),
            log_streams: self.logs.stream_count(),
            eval_entries: self.evals.len(),
        }
    }

    fn insert_capped_string(map: &mut BTreeMap<String, String>, key: String, value: String) {
        if map.len() >= MAX_ENTRIES_PER_MAP
            && !map.contains_key(&key)
            && let Some(first) = map.keys().next().cloned()
        {
            map.remove(&first);
        }
        map.insert(key, value);
    }

    fn insert_capped_value(map: &mut BTreeMap<String, Value>, key: String, value: Value) {
        if map.len() >= MAX_ENTRIES_PER_MAP
            && !map.contains_key(&key)
            && let Some(first) = map.keys().next().cloned()
        {
            map.remove(&first);
        }
        map.insert(key, value);
    }
}

/// Handle one request against mutable state.
pub fn handle_request(
    state: &mut DaemonState,
    socket: &Path,
    request: &DaemonRequest,
) -> DaemonResponse {
    if request.v != DAEMON_PROTOCOL_VERSION {
        return error_response(
            request,
            "protocol_mismatch",
            format!(
                "unsupported protocol version {} (daemon speaks {})",
                request.v, DAEMON_PROTOCOL_VERSION
            ),
        );
    }

    match request.method.as_str() {
        "hello" => ok_json(
            request,
            &DaemonHello {
                protocol_version: DAEMON_PROTOCOL_VERSION,
                role: DAEMON_ROLE.to_owned(),
                pid: std::process::id(),
                uptime_ms: Some(state.uptime_ms()),
            },
        ),
        "status" => ok_json(request, &state.status(socket)),
        "shutdown" => {
            state.stop = true;
            ok_json(request, &serde_json::json!({ "stopping": true }))
        }
        "discovery.get" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            match state.discovery.get(&key) {
                Some(payload) => ok_json(
                    request,
                    &serde_json::json!({ "hit": true, "payload": payload }),
                ),
                None => ok_json(request, &serde_json::json!({ "hit": false })),
            }
        }
        "discovery.put" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            let payload = match param_value(request, "payload") {
                Ok(payload) => payload,
                Err(resp) => return resp,
            };
            DaemonState::insert_capped_value(&mut state.discovery, key, payload);
            ok_json(request, &serde_json::json!({ "stored": true }))
        }
        "plan.get" => {
            let key = match param_str(request, "key_digest") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            match state.plans.get(&key) {
                Some(entry) => {
                    if plan_contains_secret_values(&entry.plan) {
                        state.plans.remove(&key);
                        return ok_json(request, &serde_json::json!({ "hit": false }));
                    }
                    ok_json(
                        request,
                        &serde_json::json!({
                            "hit": true,
                            "entry": entry,
                        }),
                    )
                }
                None => ok_json(request, &serde_json::json!({ "hit": false })),
            }
        }
        "plan.put" => {
            let key = match param_str(request, "key_digest") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            let entry: DaemonPlanEntry = match param_decode(request, "entry") {
                Ok(entry) => entry,
                Err(resp) => return resp,
            };
            if plan_contains_secret_values(&entry.plan) {
                return error_response(
                    request,
                    "secret_rejected",
                    "refusing to retain plan with non-placeholder secret values",
                );
            }
            if state.plans.len() >= MAX_ENTRIES_PER_MAP
                && !state.plans.contains_key(&key)
                && let Some(first) = state.plans.keys().next().cloned()
            {
                state.plans.remove(&first);
            }
            state.plans.insert(key, entry);
            ok_json(request, &serde_json::json!({ "stored": true }))
        }
        "fingerprint.get" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            match state.fingerprints.get(&key) {
                Some(value) => {
                    ok_json(request, &serde_json::json!({ "hit": true, "value": value }))
                }
                None => ok_json(request, &serde_json::json!({ "hit": false })),
            }
        }
        "fingerprint.put" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            let value = match param_str(request, "value") {
                Ok(value) => value,
                Err(resp) => return resp,
            };
            DaemonState::insert_capped_string(&mut state.fingerprints, key, value);
            ok_json(request, &serde_json::json!({ "stored": true }))
        }
        "merkle.invalidate" => {
            let root = match param_str(request, "root") {
                Ok(root) => root,
                Err(resp) => return resp,
            };
            let paths: Vec<String> = match param_decode(request, "paths") {
                Ok(paths) => paths,
                Err(resp) => return resp,
            };
            let touched = touched_directories(&paths);
            let entry = state.merkle_invalidated.entry(root).or_default();
            for path in &paths {
                entry.insert(path.clone());
            }
            while entry.len() > MAX_ENTRIES_PER_MAP {
                if let Some(first) = entry.iter().next().cloned() {
                    entry.remove(&first);
                } else {
                    break;
                }
            }
            ok_json(
                request,
                &serde_json::json!({
                    "invalidated": paths.len(),
                    "touched_directories": touched.into_iter().collect::<Vec<_>>(),
                }),
            )
        }
        "merkle.invalidated.get" => {
            let root = match param_str(request, "root") {
                Ok(root) => root,
                Err(resp) => return resp,
            };
            let paths: Vec<String> = state
                .merkle_invalidated
                .get(&root)
                .map(|set| set.iter().cloned().collect())
                .unwrap_or_default();
            ok_json(request, &serde_json::json!({ "paths": paths }))
        }
        "action_key.put" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            let digest = match param_str(request, "digest") {
                Ok(digest) => digest,
                Err(resp) => return resp,
            };
            if digest.len() > 128 || digest.contains(' ') {
                return error_response(
                    request,
                    "invalid_digest",
                    "action_key digest must be a compact hex digest",
                );
            }
            DaemonState::insert_capped_string(&mut state.action_keys, key, digest);
            ok_json(request, &serde_json::json!({ "stored": true }))
        }
        "action_key.get" => {
            let key = match param_str(request, "key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            match state.action_keys.get(&key) {
                Some(digest) => ok_json(
                    request,
                    &serde_json::json!({ "hit": true, "digest": digest }),
                ),
                None => ok_json(request, &serde_json::json!({ "hit": false })),
            }
        }
        "log.open" => {
            let stream = match param_str(request, "stream") {
                Ok(stream) => stream,
                Err(resp) => return resp,
            };
            let path = match optional_param_str(request, "path") {
                Ok(path) => path.map(PathBuf::from),
                Err(resp) => return resp,
            };
            if let Err(message) = state.logs.open(stream.clone(), path) {
                return error_response(request, "invalid_params", message);
            }
            ok_json(
                request,
                &serde_json::json!({ "opened": true, "stream": stream }),
            )
        }
        "log.append" => {
            let stream = match param_str(request, "stream") {
                Ok(stream) => stream,
                Err(resp) => return resp,
            };
            let encoded = match param_str(request, "data_b64") {
                Ok(encoded) => encoded,
                Err(resp) => return resp,
            };
            let data = match decode_log_bytes(&encoded) {
                Ok(data) => data,
                Err(message) => {
                    return error_response(request, "invalid_params", message);
                }
            };
            match state.logs.append(&stream, &data) {
                Ok(written) => ok_json(
                    request,
                    &serde_json::json!({ "written": written, "stream": stream }),
                ),
                Err(message) => error_response(request, "invalid_params", message),
            }
        }
        "log.close" => {
            let stream = match param_str(request, "stream") {
                Ok(stream) => stream,
                Err(resp) => return resp,
            };
            state.logs.close(&stream);
            ok_json(
                request,
                &serde_json::json!({ "closed": true, "stream": stream }),
            )
        }
        // `log.subscribe` is handled in the connection loop (streaming).
        "log.subscribe" => error_response(
            request,
            "invalid_params",
            "log.subscribe must be handled as a streaming connection method",
        ),
        "eval.prepare" => {
            let params: EvalPrepareParams =
                match serde_json::from_value(request.params.clone().unwrap_or(Value::Null)) {
                    Ok(params) => params,
                    Err(error) => {
                        return error_response(
                            request,
                            "invalid_params",
                            format!("eval.prepare params: {error}"),
                        );
                    }
                };
            if params.nix_identity.trim().is_empty()
                || params.flake_root.trim().is_empty()
                || params.flake_fingerprint.trim().is_empty()
            {
                return error_response(
                    request,
                    "invalid_params",
                    "eval.prepare requires nix_identity, flake_root, and flake_fingerprint",
                );
            }
            let invalidated = state.evals.prepare(&params);
            ok_json(
                request,
                &serde_json::json!({
                    "prepared": true,
                    "invalidated": invalidated,
                    "entries": state.evals.len(),
                }),
            )
        }
        "eval.get" => {
            let params: EvalPrepareParams = match eval_session_params(request) {
                Ok(params) => params,
                Err(resp) => return resp,
            };
            let kind = match eval_kind_param(request) {
                Ok(kind) => kind,
                Err(resp) => return resp,
            };
            let cache_key = match param_str(request, "cache_key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            match state.evals.get(&params, kind, &cache_key) {
                Some(json) => ok_json(request, &serde_json::json!({ "hit": true, "json": json })),
                None => ok_json(request, &serde_json::json!({ "hit": false })),
            }
        }
        "eval.put" => {
            let params: EvalPrepareParams = match eval_session_params(request) {
                Ok(params) => params,
                Err(resp) => return resp,
            };
            let kind = match eval_kind_param(request) {
                Ok(kind) => kind,
                Err(resp) => return resp,
            };
            let cache_key = match param_str(request, "cache_key") {
                Ok(key) => key,
                Err(resp) => return resp,
            };
            let json = match param_value(request, "json") {
                Ok(json) => json,
                Err(resp) => return resp,
            };
            match state.evals.put(&params, kind, &cache_key, json) {
                Ok(()) => ok_json(
                    request,
                    &serde_json::json!({ "stored": true, "entries": state.evals.len() }),
                ),
                Err(message) => error_response(request, "invalid_params", message),
            }
        }
        // Reserved for future remote-worker registry — refuse so clients fall back.
        "worker.register" => error_response(
            request,
            "not_implemented",
            "method worker.register reserved; daemon is cache/coordination only",
        ),
        other => error_response(request, "unknown_method", format!("unknown method {other}")),
    }
}

fn eval_session_params(request: &DaemonRequest) -> Result<EvalPrepareParams, DaemonResponse> {
    let Some(params) = request.params.as_ref() else {
        return Err(error_response(
            request,
            "invalid_params",
            "missing params object",
        ));
    };
    let session = params.get("session").cloned().unwrap_or_else(|| {
        serde_json::json!({
            "nix_identity": params.get("nix_identity").cloned().unwrap_or(Value::Null),
            "config_fingerprint": params.get("config_fingerprint").cloned(),
            "flake_root": params.get("flake_root").cloned().unwrap_or(Value::Null),
            "flake_fingerprint": params.get("flake_fingerprint").cloned().unwrap_or(Value::Null),
        })
    });
    serde_json::from_value(session).map_err(|error| {
        error_response(
            request,
            "invalid_params",
            format!("eval session params: {error}"),
        )
    })
}

fn eval_kind_param(request: &DaemonRequest) -> Result<EvalKind, DaemonResponse> {
    let kind = param_str(request, "kind")?;
    EvalKind::parse(&kind).ok_or_else(|| {
        error_response(
            request,
            "invalid_params",
            format!("unsupported eval kind `{kind}` (expected metadata|tasks|list)"),
        )
    })
}

fn ok_json<T: Serialize>(request: &DaemonRequest, value: &T) -> DaemonResponse {
    DaemonResponse {
        v: DAEMON_PROTOCOL_VERSION,
        id: request.id,
        ok: true,
        result: serde_json::to_value(value).ok(),
        error: None,
    }
}

fn error_response(
    request: &DaemonRequest,
    code: &str,
    message: impl Into<String>,
) -> DaemonResponse {
    DaemonResponse {
        v: DAEMON_PROTOCOL_VERSION,
        id: request.id,
        ok: false,
        result: None,
        error: Some(DaemonErrorBody {
            code: code.to_owned(),
            message: message.into(),
        }),
    }
}

fn param_str(request: &DaemonRequest, field: &str) -> Result<String, DaemonResponse> {
    let Some(params) = request.params.as_ref() else {
        return Err(error_response(
            request,
            "invalid_params",
            format!("missing params.{field}"),
        ));
    };
    match params.get(field).and_then(Value::as_str) {
        Some(value) => Ok(value.to_owned()),
        None => Err(error_response(
            request,
            "invalid_params",
            format!("missing or non-string params.{field}"),
        )),
    }
}

fn optional_param_str(
    request: &DaemonRequest,
    field: &str,
) -> Result<Option<String>, DaemonResponse> {
    let Some(params) = request.params.as_ref() else {
        return Ok(None);
    };
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(error_response(
            request,
            "invalid_params",
            format!("params.{field} must be a string when present"),
        )),
    }
}

fn optional_param_bool(request: &DaemonRequest, field: &str, default: bool) -> bool {
    request
        .params
        .as_ref()
        .and_then(|params| params.get(field))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn param_value(request: &DaemonRequest, field: &str) -> Result<Value, DaemonResponse> {
    let Some(params) = request.params.as_ref() else {
        return Err(error_response(
            request,
            "invalid_params",
            format!("missing params.{field}"),
        ));
    };
    match params.get(field) {
        Some(value) => Ok(value.clone()),
        None => Err(error_response(
            request,
            "invalid_params",
            format!("missing params.{field}"),
        )),
    }
}

fn param_decode<T: for<'de> Deserialize<'de>>(
    request: &DaemonRequest,
    field: &str,
) -> Result<T, DaemonResponse> {
    let value = param_value(request, field)?;
    serde_json::from_value(value).map_err(|error| {
        error_response(
            request,
            "invalid_params",
            format!("params.{field}: {error}"),
        )
    })
}

/// Ensure parent directories exist for the socket path.
///
/// # Errors
///
/// Returns [`io::Error`] when directories cannot be created.
pub fn ensure_socket_parent(socket: &Path) -> io::Result<()> {
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Write the daemon PID file.
///
/// # Errors
///
/// Returns [`io::Error`] when the file cannot be written.
pub fn write_pid_file(socket: &Path) -> io::Result<()> {
    let path = daemon_pid_path(socket);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(())
}

/// Remove socket and PID files (best-effort).
pub fn cleanup_socket_files(socket: &Path) {
    let _ = fs::remove_file(socket);
    let _ = fs::remove_file(daemon_pid_path(socket));
}

/// Read a PID from the sidecar file when present.
#[must_use]
pub fn read_pid_file(socket: &Path) -> Option<u32> {
    let contents = fs::read_to_string(daemon_pid_path(socket)).ok()?;
    contents.trim().parse().ok()
}

/// Convert a daemon plan entry into the disk-cache hit shape used by prepare.
#[must_use]
pub fn daemon_plan_to_hit(entry: DaemonPlanEntry) -> PreparedPlanCacheHit {
    PreparedPlanCacheHit {
        prepare_kind: entry.prepare_kind,
        plan: entry.plan,
        nix: entry.nix,
        execution_directory: entry.execution_directory,
        fingerprints: entry.fingerprints,
    }
}

/// Build a daemon plan entry from prepare outputs (rejects secret values).
#[must_use]
pub fn daemon_plan_entry(
    prepare_kind: PlanPrepareKind,
    plan: &Plan,
    nix: &str,
    execution_directory: &str,
    fingerprints: PlanCacheSharedFingerprints,
) -> Option<DaemonPlanEntry> {
    if plan_contains_secret_values(plan) {
        return None;
    }
    let mut plan = plan.clone();
    for secret in &mut plan.secrets {
        scrub_secret(secret);
    }
    Some(DaemonPlanEntry {
        prepare_kind,
        plan,
        nix: nix.to_owned(),
        execution_directory: execution_directory.to_owned(),
        fingerprints,
    })
}

fn scrub_secret(secret: &mut PlanSecretRef) {
    let trimmed = secret.value.trim();
    if !trimmed.is_empty() && trimmed != PLAN_SECRET_RUNTIME_PLACEHOLDER {
        secret.value = PLAN_SECRET_RUNTIME_PLACEHOLDER.to_owned();
    }
}

/// Client-side errors when talking to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonClientError {
    #[error("daemon connect disabled (NXR_DAEMON)")]
    Disabled,
    #[error("daemon socket absent")]
    Absent,
    #[error("protocol mismatch: {0}")]
    ProtocolMismatch(String),
    #[error("daemon error {code}: {message}")]
    Remote { code: String, message: String },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Connected daemon client (one request at a time on this stream).
pub struct DaemonConnection {
    #[cfg(unix)]
    stream: std::os::unix::net::UnixStream,
    next_id: u64,
}

/// Try to connect and complete a hello handshake.
///
/// Returns [`DaemonClientError::Disabled`] / [`Absent`] / [`ProtocolMismatch`]
/// so callers can fall back to standalone mode.
pub fn try_connect(socket: &Path) -> Result<DaemonConnection, DaemonClientError> {
    if !daemon_connect_enabled() {
        return Err(DaemonClientError::Disabled);
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        return Err(DaemonClientError::Absent);
    }
    #[cfg(unix)]
    {
        if !socket.exists() {
            return Err(DaemonClientError::Absent);
        }
        let stream = std::os::unix::net::UnixStream::connect(socket).map_err(|error| {
            if error.kind() == io::ErrorKind::ConnectionRefused
                || error.kind() == io::ErrorKind::NotFound
            {
                DaemonClientError::Absent
            } else {
                DaemonClientError::Io(error)
            }
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut conn = DaemonConnection { stream, next_id: 1 };
        let hello: DaemonHello = conn.call("hello", None)?;
        if hello.protocol_version != DAEMON_PROTOCOL_VERSION {
            return Err(DaemonClientError::ProtocolMismatch(format!(
                "daemon protocol {} != client {}",
                hello.protocol_version, DAEMON_PROTOCOL_VERSION
            )));
        }
        if hello.role != DAEMON_ROLE {
            return Err(DaemonClientError::ProtocolMismatch(format!(
                "unexpected daemon role {}",
                hello.role
            )));
        }
        Ok(conn)
    }
}

impl DaemonConnection {
    /// Send a method call and decode the `result` object.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonClientError`] on I/O, JSON, protocol, or remote errors.
    pub fn call<T: for<'de> Deserialize<'de>>(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, DaemonClientError> {
        let response = self.call_raw(method, params)?;
        if !response.ok {
            let error = response.error.unwrap_or(DaemonErrorBody {
                code: "unknown".to_owned(),
                message: "daemon request failed".to_owned(),
            });
            if error.code == "protocol_mismatch" {
                return Err(DaemonClientError::ProtocolMismatch(error.message));
            }
            return Err(DaemonClientError::Remote {
                code: error.code,
                message: error.message,
            });
        }
        let value = response.result.unwrap_or(Value::Null);
        Ok(serde_json::from_value(value)?)
    }

    /// Raw request/response exchange.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonClientError`] on I/O or JSON failures.
    pub fn call_raw(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<DaemonResponse, DaemonClientError> {
        #[cfg(not(unix))]
        {
            let _ = (method, params);
            return Err(DaemonClientError::Absent);
        }
        #[cfg(unix)]
        {
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            let request = DaemonRequest {
                v: DAEMON_PROTOCOL_VERSION,
                id,
                method: method.to_owned(),
                params,
            };
            let mut line = serde_json::to_string(&request)?;
            line.push('\n');
            self.stream.write_all(line.as_bytes())?;
            self.stream.flush()?;
            let mut reader = BufReader::new(self.stream.try_clone()?);
            let mut response_line = String::new();
            reader.read_line(&mut response_line)?;
            if response_line.is_empty() {
                return Err(DaemonClientError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "daemon closed connection",
                )));
            }
            Ok(serde_json::from_str(response_line.trim())?)
        }
    }

    /// Subscribe to a log stream and invoke `on_chunk` for each payload until
    /// EOF, disconnect, or `should_continue` returns false after an idle poll.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonClientError`] on I/O, JSON, or remote errors.
    #[cfg(unix)]
    pub fn follow_log_stream<F, C>(
        &mut self,
        stream: &str,
        path: Option<&str>,
        from_start: bool,
        mut on_chunk: F,
        mut should_continue: C,
    ) -> Result<(), DaemonClientError>
    where
        F: FnMut(&[u8]) -> Result<(), DaemonClientError>,
        C: FnMut() -> bool,
    {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut params = serde_json::json!({
            "stream": stream,
            "from_start": from_start,
        });
        if let Some(path) = path {
            params
                .as_object_mut()
                .expect("object")
                .insert("path".to_owned(), Value::String(path.to_owned()));
        }
        let request = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id,
            method: "log.subscribe".to_owned(),
            params: Some(params),
        };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;

        // Idle reads wake often enough for the caller to observe process death.
        self.stream
            .set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut reader = BufReader::new(self.stream.try_clone()?);
        let mut idle_after_exit = 0u8;

        loop {
            let mut response_line = String::new();
            match reader.read_line(&mut response_line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut =>
                {
                    if !should_continue() {
                        idle_after_exit = idle_after_exit.saturating_add(1);
                        // One extra idle wake lets in-flight file chunks arrive.
                        if idle_after_exit >= 2 {
                            break;
                        }
                    } else {
                        idle_after_exit = 0;
                    }
                    continue;
                }
                Err(error) => return Err(DaemonClientError::Io(error)),
            }
            if response_line.trim().is_empty() {
                continue;
            }
            let response: DaemonResponse = serde_json::from_str(response_line.trim())?;
            if !response.ok {
                let error = response.error.unwrap_or(DaemonErrorBody {
                    code: "unknown".to_owned(),
                    message: "log.subscribe failed".to_owned(),
                });
                return Err(DaemonClientError::Remote {
                    code: error.code,
                    message: error.message,
                });
            }
            let Some(result) = response.result else {
                continue;
            };
            if result.get("subscribed").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            match result.get("type").and_then(Value::as_str) {
                Some("chunk") => {
                    let encoded =
                        result
                            .get("data_b64")
                            .and_then(Value::as_str)
                            .ok_or_else(|| DaemonClientError::Remote {
                                code: "invalid_params".to_owned(),
                                message: "chunk missing data_b64".to_owned(),
                            })?;
                    let data =
                        decode_log_bytes(encoded).map_err(|message| DaemonClientError::Remote {
                            code: "invalid_params".to_owned(),
                            message,
                        })?;
                    on_chunk(&data)?;
                    idle_after_exit = 0;
                }
                Some("eof") => break,
                _ => {}
            }
        }
        Ok(())
    }
}

/// Convenience: one-shot call against the default or override socket.
///
/// Returns `None` when the daemon is absent, disabled, or mismatched so callers
/// fall back silently.
#[must_use]
pub fn try_once<T: for<'de> Deserialize<'de>>(method: &str, params: Option<Value>) -> Option<T> {
    let socket = daemon_socket_path();
    let mut conn = try_connect(&socket).ok()?;
    conn.call(method, params).ok()
}

/// Shared state handle for the serve loop.
pub type SharedDaemonState = Arc<Mutex<DaemonState>>;

/// Serve JSON-lines requests until shutdown (Unix).
///
/// Each accepted connection runs on its own thread so long-lived
/// `log.subscribe` streams do not block the accept loop.
///
/// # Errors
///
/// Returns [`io::Error`] when the listener cannot bind or accept fails fatally.
pub fn serve(socket: &Path, state: SharedDaemonState) -> io::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (socket, state);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "nxrd requires Unix sockets",
        ));
    }
    #[cfg(unix)]
    {
        ensure_socket_parent(socket)?;
        cleanup_socket_files(socket);
        let listener = std::os::unix::net::UnixListener::bind(socket)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(socket, fs::Permissions::from_mode(0o600));
        }
        write_pid_file(socket)?;

        // Accept loop: spawn per connection; poll stop between accepts with a
        // short timeout so shutdown does not wait forever for a new client.
        listener.set_nonblocking(true)?;
        loop {
            let stop = state.lock().map(|guard| guard.stop).unwrap_or(true);
            if stop {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
                    let conn_state = Arc::clone(&state);
                    let conn_socket = socket.to_path_buf();
                    std::thread::spawn(move || {
                        let _ = handle_connection(&conn_socket, &conn_state, stream);
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    cleanup_socket_files(socket);
                    return Err(error);
                }
            }
        }
        cleanup_socket_files(socket);
        Ok(())
    }
}

#[cfg(unix)]
fn handle_connection(
    socket: &Path,
    state: &SharedDaemonState,
    stream: std::os::unix::net::UnixStream,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: DaemonRequest = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(error) => {
                let response = DaemonResponse {
                    v: DAEMON_PROTOCOL_VERSION,
                    id: 0,
                    ok: false,
                    result: None,
                    error: Some(DaemonErrorBody {
                        code: "invalid_json".to_owned(),
                        message: error.to_string(),
                    }),
                };
                write_response(&mut writer, &response)?;
                continue;
            }
        };
        if request.method == "log.subscribe" {
            handle_log_subscribe(state, &mut writer, &request)?;
            break;
        }
        let response = {
            let mut guard = state
                .lock()
                .map_err(|_| io::Error::other("daemon state mutex poisoned"))?;
            handle_request(&mut guard, socket, &request)
        };
        write_response(&mut writer, &response)?;
        if request.method == "shutdown" {
            break;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn handle_log_subscribe(
    state: &SharedDaemonState,
    writer: &mut std::os::unix::net::UnixStream,
    request: &DaemonRequest,
) -> io::Result<()> {
    if request.v != DAEMON_PROTOCOL_VERSION {
        let response = error_response(
            request,
            "protocol_mismatch",
            format!(
                "unsupported protocol version {} (daemon speaks {})",
                request.v, DAEMON_PROTOCOL_VERSION
            ),
        );
        return write_response(writer, &response);
    }

    let stream_id = match param_str(request, "stream") {
        Ok(stream) => stream,
        Err(resp) => return write_response(writer, &resp),
    };
    let path_param = match optional_param_str(request, "path") {
        Ok(path) => path.map(PathBuf::from),
        Err(resp) => return write_response(writer, &resp),
    };
    let from_start = optional_param_bool(request, "from_start", true);

    let (tail, rx, file_path) = {
        let mut guard = state
            .lock()
            .map_err(|_| io::Error::other("daemon state mutex poisoned"))?;
        if let Some(path) = path_param.clone() {
            if let Err(message) = guard.logs.open(stream_id.clone(), Some(path)) {
                let response = error_response(request, "invalid_params", message);
                return write_response(writer, &response);
            }
        } else if let Err(message) = guard.logs.open(stream_id.clone(), None) {
            let response = error_response(request, "invalid_params", message);
            return write_response(writer, &response);
        }
        let (tail, rx) = match guard.logs.subscribe(&stream_id) {
            Ok(pair) => pair,
            Err(message) => {
                let response = error_response(request, "invalid_params", message);
                return write_response(writer, &response);
            }
        };
        let file_path = path_param.or_else(|| guard.logs.path_for(&stream_id));
        (tail, rx, file_path)
    };

    let subscribed = ok_json(
        request,
        &serde_json::json!({
            "subscribed": true,
            "stream": stream_id,
            "from_start": from_start,
        }),
    );
    write_response(writer, &subscribed)?;

    // File-backed follow streams the open FD; the RAM tail is for append-only
    // producers (avoid duplicating disk bytes).
    if file_path.is_none() && !tail.is_empty() {
        write_chunk_event(writer, request.id, &tail)?;
    }

    let mut file = match file_path.as_ref() {
        Some(path) => match File::open(path) {
            Ok(mut file) => {
                if from_start {
                    file.seek(SeekFrom::Start(0))?;
                } else {
                    file.seek(SeekFrom::End(0))?;
                }
                Some(file)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        },
        None => None,
    };

    // Stream existing file bytes first (may exceed the bounded RAM tail).
    if let Some(file) = file.as_mut()
        && from_start
    {
        let mut buffer = [0u8; 8192];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            write_chunk_event(writer, request.id, &buffer[..read])?;
        }
    }

    let poll = Duration::from_millis(FILE_POLL_MS);
    let mut buffer = [0u8; 8192];
    loop {
        let mut progress = false;
        if let Some(file) = file.as_mut() {
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                progress = true;
                write_chunk_event(writer, request.id, &buffer[..read])?;
            }
        }

        // Drain append-fed events when this stream has no file FD (file is the
        // source of truth for process logs).
        if file.is_none() {
            loop {
                match rx.try_recv() {
                    Ok(LogEvent::Chunk(data)) => {
                        progress = true;
                        write_chunk_event(writer, request.id, &data)?;
                    }
                    Ok(LogEvent::Closed) => {
                        let eof = ok_json(
                            request,
                            &serde_json::json!({ "type": "eof", "stream": stream_id }),
                        );
                        write_response(writer, &eof)?;
                        return Ok(());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }
        }

        if progress {
            continue;
        }

        match rx.recv_timeout(poll) {
            Ok(LogEvent::Chunk(data)) => {
                if file.is_none() {
                    write_chunk_event(writer, request.id, &data)?;
                }
            }
            Ok(LogEvent::Closed) => {
                let eof = ok_json(
                    request,
                    &serde_json::json!({ "type": "eof", "stream": stream_id }),
                );
                write_response(writer, &eof)?;
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn write_chunk_event(writer: &mut impl Write, id: u64, data: &[u8]) -> io::Result<()> {
    let response = DaemonResponse {
        v: DAEMON_PROTOCOL_VERSION,
        id,
        ok: true,
        result: Some(serde_json::json!({
            "type": "chunk",
            "data_b64": encode_log_bytes(data),
        })),
        error: None,
    };
    write_response(writer, &response)
}

fn write_response(writer: &mut impl Write, response: &DaemonResponse) -> io::Result<()> {
    let mut line = serde_json::to_string(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// Epoch seconds helper for status / diagnostics.
#[must_use]
pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_broker::encode_log_bytes;
    use crate::plan::{PlanCommand, PlanKind};
    use crate::plan_cache::PlanPrepareKind;

    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::sync::Mutex;
    #[cfg(unix)]
    use std::thread;

    fn sample_plan() -> Plan {
        Plan {
            schema_version: Plan::SCHEMA_VERSION,
            kind: PlanKind::App,
            flake: "/tmp/flake".to_owned(),
            system: "aarch64-darwin".to_owned(),
            target: "test".to_owned(),
            attr_path: "apps.aarch64-darwin.test".to_owned(),
            invocation_directory: "/tmp/flake".to_owned(),
            execution_directory: "/tmp/flake".to_owned(),
            shell: None,
            active_shell: None,
            environment_policy: crate::EnvironmentPolicy::Inherit,
            context: None,
            secrets: Vec::new(),
            context_env_set: BTreeMap::new(),
            command: PlanCommand {
                program: "nix".to_owned(),
                arguments: vec!["run".to_owned(), ".#test".to_owned()],
            },
            forwarded_arguments: Vec::new(),
        }
    }

    fn sample_fingerprints() -> PlanCacheSharedFingerprints {
        PlanCacheSharedFingerprints {
            nix_tree_fingerprint: "a".to_owned(),
            discovery_inputs_fingerprint: "b".to_owned(),
            flake_lock_digest: None,
            nix_path: "/nix/var/nix".to_owned(),
            nix_version: "2.24.0".to_owned(),
            nix_file_identity: None,
            source_identity: None,
        }
    }

    #[test]
    fn kill_switch_parser() {
        assert!(daemon_connect_enabled_for(None));
        assert!(!daemon_connect_enabled_for(Some("off")));
        assert!(!daemon_connect_enabled_for(Some("0")));
        assert!(!daemon_connect_enabled_for(Some("false")));
        assert!(!daemon_connect_enabled_for(Some("no")));
        assert!(daemon_connect_enabled_for(Some("on")));
    }

    #[test]
    fn handle_request_plan_round_trip_and_secret_reject() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let entry = daemon_plan_entry(
            PlanPrepareKind::Fast,
            &sample_plan(),
            "nix",
            "/tmp/flake",
            sample_fingerprints(),
        )
        .expect("entry");
        let put = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 1,
            method: "plan.put".to_owned(),
            params: Some(serde_json::json!({
                "key_digest": "abc",
                "entry": entry,
            })),
        };
        assert!(handle_request(&mut state, &socket, &put).ok);

        let get = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 2,
            method: "plan.get".to_owned(),
            params: Some(serde_json::json!({ "key_digest": "abc" })),
        };
        let get_resp = handle_request(&mut state, &socket, &get);
        assert!(get_resp.ok);
        assert!(get_resp.result.unwrap()["hit"].as_bool().unwrap());

        let mut secret_plan = sample_plan();
        secret_plan.secrets.push(PlanSecretRef {
            name: "TOKEN".to_owned(),
            reference: "TOKEN".to_owned(),
            delivery: "env".to_owned(),
            provider: "env".to_owned(),
            value: "super-secret".to_owned(),
        });
        assert!(
            daemon_plan_entry(
                PlanPrepareKind::Fast,
                &secret_plan,
                "nix",
                "/tmp",
                sample_fingerprints(),
            )
            .is_none()
        );
    }

    #[test]
    fn merkle_invalidate_records_paths() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let req = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 1,
            method: "merkle.invalidate".to_owned(),
            params: Some(serde_json::json!({
                "root": "/repo",
                "paths": ["apps/foo/main.rs", "apps/bar/lib.rs"],
            })),
        };
        assert!(handle_request(&mut state, &socket, &req).ok);
        let get = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 2,
            method: "merkle.invalidated.get".to_owned(),
            params: Some(serde_json::json!({ "root": "/repo" })),
        };
        let get_resp = handle_request(&mut state, &socket, &get);
        assert!(get_resp.ok);
        assert_eq!(
            get_resp.result.unwrap()["paths"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn protocol_mismatch_refuses() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let req = DaemonRequest {
            v: 999,
            id: 1,
            method: "hello".to_owned(),
            params: None,
        };
        let resp = handle_request(&mut state, &socket, &req);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "protocol_mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_round_trip() {
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
        assert!(socket.exists(), "daemon socket should appear");

        let mut conn = try_connect(&socket).expect("connect");
        let status: DaemonStatus = conn.call("status", None).expect("status");
        assert_eq!(status.protocol_version, DAEMON_PROTOCOL_VERSION);
        assert_eq!(status.role, DAEMON_ROLE);

        let _: Value = conn
            .call(
                "discovery.put",
                Some(serde_json::json!({
                    "key": "k1",
                    "payload": { "apps": ["hello"] },
                })),
            )
            .expect("put");
        let got: Value = conn
            .call("discovery.get", Some(serde_json::json!({ "key": "k1" })))
            .expect("get");
        assert_eq!(got["hit"], true);

        let _: Value = conn.call("shutdown", None).expect("shutdown");
        let _ = server.join();
    }

    #[test]
    fn log_open_append_status_counts_streams() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let open = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 1,
            method: "log.open".to_owned(),
            params: Some(serde_json::json!({ "stream": "proj/api" })),
        };
        assert!(handle_request(&mut state, &socket, &open).ok);
        let append = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 2,
            method: "log.append".to_owned(),
            params: Some(serde_json::json!({
                "stream": "proj/api",
                "data_b64": encode_log_bytes(b"hello-broker"),
            })),
        };
        let append_resp = handle_request(&mut state, &socket, &append);
        assert!(append_resp.ok);
        assert_eq!(append_resp.result.unwrap()["written"], 12);
        let status = state.status(&socket);
        assert_eq!(status.log_streams, 1);
    }

    #[cfg(unix)]
    #[test]
    fn log_subscribe_receives_append_chunks() {
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

        let append_socket = socket.clone();
        let producer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            let mut conn = try_connect(&append_socket).expect("producer connect");
            let _: Value = conn
                .call(
                    "log.append",
                    Some(serde_json::json!({
                        "stream": "proj/worker",
                        "data_b64": encode_log_bytes(b"line-one\n"),
                    })),
                )
                .expect("append");
            let _: Value = conn
                .call(
                    "log.close",
                    Some(serde_json::json!({ "stream": "proj/worker" })),
                )
                .expect("close");
        });

        let mut conn = try_connect(&socket).expect("subscriber connect");
        let mut body = Vec::new();
        conn.follow_log_stream(
            "proj/worker",
            None,
            true,
            |chunk| {
                body.extend_from_slice(chunk);
                Ok(())
            },
            || true,
        )
        .expect("follow");
        producer.join().expect("producer");
        assert_eq!(String::from_utf8(body).unwrap(), "line-one\n");

        let mut shutdown = try_connect(&socket).expect("shutdown connect");
        let _: Value = shutdown.call("shutdown", None).expect("shutdown");
        let _ = server.join();
    }

    #[cfg(unix)]
    #[test]
    fn log_subscribe_follows_open_file_fd() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nxrd.sock");
        let log_path = dir.path().join("worker.log");
        fs::write(&log_path, b"seed\n").unwrap();

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

        let log_path_for_writer = log_path.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            let mut file = OpenOptions::new()
                .append(true)
                .open(&log_path_for_writer)
                .unwrap();
            file.write_all(b"live\n").unwrap();
        });

        let mut conn = try_connect(&socket).expect("connect");
        let mut body = Vec::new();
        let saw_live = std::sync::atomic::AtomicBool::new(false);
        conn.follow_log_stream(
            "proj/file",
            Some(log_path.to_str().unwrap()),
            true,
            |chunk| {
                body.extend_from_slice(chunk);
                if body.windows(5).any(|w| w == b"live\n") {
                    saw_live.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(())
            },
            || !saw_live.load(std::sync::atomic::Ordering::SeqCst),
        )
        .expect("follow");
        writer.join().unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("seed\n"), "got {text:?}");
        assert!(text.contains("live\n"), "got {text:?}");

        let mut shutdown = try_connect(&socket).expect("shutdown connect");
        let _: Value = shutdown.call("shutdown", None).expect("shutdown");
        let _ = server.join();
    }

    #[test]
    fn eval_prepare_get_put_round_trip() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let session = serde_json::json!({
            "nix_identity": "nix@1",
            "config_fingerprint": "cfg",
            "flake_root": "/repo",
            "flake_fingerprint": "fp1",
        });
        let prepare = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 1,
            method: "eval.prepare".to_owned(),
            params: Some(session.clone()),
        };
        let prepare_resp = handle_request(&mut state, &socket, &prepare);
        assert!(prepare_resp.ok);
        assert_eq!(prepare_resp.result.unwrap()["prepared"], true);

        let put = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 2,
            method: "eval.put".to_owned(),
            params: Some(serde_json::json!({
                "session": session,
                "kind": "metadata",
                "cache_key": "meta:fp1",
                "json": { "schema_version": 1, "inventory": { "apps": ["hello"] } },
            })),
        };
        assert!(handle_request(&mut state, &socket, &put).ok);

        let get = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 3,
            method: "eval.get".to_owned(),
            params: Some(serde_json::json!({
                "session": session,
                "kind": "metadata",
                "cache_key": "meta:fp1",
            })),
        };
        let get_resp = handle_request(&mut state, &socket, &get);
        assert!(get_resp.ok);
        let result = get_resp.result.unwrap();
        assert_eq!(result["hit"], true);
        assert_eq!(result["json"]["schema_version"], 1);
        assert_eq!(state.status(&socket).eval_entries, 1);

        let prepare2 = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 4,
            method: "eval.prepare".to_owned(),
            params: Some(serde_json::json!({
                "nix_identity": "nix@1",
                "config_fingerprint": "cfg",
                "flake_root": "/repo",
                "flake_fingerprint": "fp2",
            })),
        };
        let prep2 = handle_request(&mut state, &socket, &prepare2);
        assert!(prep2.ok);
        assert_eq!(prep2.result.unwrap()["invalidated"], true);
        assert_eq!(state.evals.len(), 0);
    }

    #[test]
    fn worker_register_still_not_implemented() {
        let mut state = DaemonState::new();
        let socket = PathBuf::from("/tmp/nxr-test.sock");
        let req = DaemonRequest {
            v: DAEMON_PROTOCOL_VERSION,
            id: 1,
            method: "worker.register".to_owned(),
            params: None,
        };
        let resp = handle_request(&mut state, &socket, &req);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "not_implemented");
    }
}
