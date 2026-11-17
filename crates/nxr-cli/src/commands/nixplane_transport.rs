//! Spawn the existing `nixplane execution` CLI as an [`ExecutionTransport`].
//!
//! No NixPlane crate dependency and no HTTP client. Argv assembly stays pure so
//! unit tests do not need the binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use nxr_task::ExecutionTransport;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

/// Speaks `nixplane execution` over a local process.
#[derive(Clone, Debug)]
pub struct NixplaneCliTransport {
    /// Binary path or name. Callers default this to `nixplane`.
    pub bin: String,
    /// Optional enrollment directory passed as `--state-dir`.
    pub state_dir: Option<String>,
}

/// Assemble `execution submit` argv after the binary name.
#[must_use]
pub fn submit_argv(
    spec: &Path,
    source_set: &Path,
    dirty_manifest: Option<&Path>,
    source_locator: Option<&str>,
    state_dir: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "execution".to_owned(),
        "submit".to_owned(),
        spec.display().to_string(),
        "--source-set".to_owned(),
        source_set.display().to_string(),
    ];
    if let Some(path) = dirty_manifest {
        argv.push("--dirty-manifest".to_owned());
        argv.push(path.display().to_string());
    }
    if let Some(locator) = source_locator {
        argv.push("--source-locator".to_owned());
        argv.push(locator.to_owned());
    }
    if let Some(dir) = state_dir {
        argv.push("--state-dir".to_owned());
        argv.push(dir.to_owned());
    }
    argv
}

fn status_argv(attempt_id: &str, state_dir: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "execution".to_owned(),
        "status".to_owned(),
        attempt_id.to_owned(),
    ];
    if let Some(dir) = state_dir {
        argv.push("--state-dir".to_owned());
        argv.push(dir.to_owned());
    }
    argv
}

fn cancel_argv(attempt_id: &str, state_dir: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "execution".to_owned(),
        "cancel".to_owned(),
        attempt_id.to_owned(),
    ];
    if let Some(dir) = state_dir {
        argv.push("--state-dir".to_owned());
        argv.push(dir.to_owned());
    }
    argv
}

/// Assemble `execution fetch` argv after the binary name.
#[must_use]
pub fn fetch_argv(
    attempt_id: &str,
    name: &str,
    out: &Path,
    state_dir: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "execution".to_owned(),
        "fetch".to_owned(),
        attempt_id.to_owned(),
        "--name".to_owned(),
        name.to_owned(),
        "--out".to_owned(),
        out.display().to_string(),
    ];
    if let Some(dir) = state_dir {
        argv.push("--state-dir".to_owned());
        argv.push(dir.to_owned());
    }
    argv
}

/// Normalize NixPlane cancel JSON so `status: "cancelled"` is an acknowledgement.
///
/// Does not set `process_terminal` or claim the worker exited. Callers still
/// treat `cancelled: true` as the acknowledgement bit for `cancel_and_bind`.
#[must_use]
pub fn normalize_cancel_response(value: &Value) -> Value {
    let mut out = value.clone();
    let status_cancelled = value.get("status").and_then(Value::as_str) == Some("cancelled");
    let already = value.get("cancelled").and_then(Value::as_bool) == Some(true);
    if let Some(object) = out.as_object_mut() {
        if status_cancelled || already {
            object.insert("cancelled".to_owned(), json!(true));
        }
    }
    out
}

impl NixplaneCliTransport {
    /// Fetch one named output. Stdout is the digest printed by `nixplane`.
    ///
    /// # Errors
    ///
    /// Returns the process error when `nixplane` cannot be started or exits non-zero.
    pub fn fetch_output(&self, attempt_id: &str, name: &str, out: &Path) -> Result<String, String> {
        let argv = fetch_argv(attempt_id, name, out, self.state_dir.as_deref());
        let output = Command::new(&self.bin)
            .args(&argv)
            .output()
            .map_err(|err| format!("nixplane: {err}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(if stderr.is_empty() {
                format!("nixplane exited {}", output.status)
            } else {
                stderr
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Replay the attempt journal from sequence 0.
    ///
    /// # Errors
    ///
    /// Returns the transport error.
    pub fn replay_journal(&self, attempt_id: &str) -> Result<Value, String> {
        self.replay_from(attempt_id, 0)
    }

    /// Replay the attempt journal after `after_sequence` (pass stored `last_sequence`).
    ///
    /// # Errors
    ///
    /// Returns the transport error.
    pub fn replay_from(&self, attempt_id: &str, after_sequence: u64) -> Result<Value, String> {
        self.call(
            "replay",
            &json!({ "attempt_id": attempt_id, "after_sequence": after_sequence }),
        )
    }
}

/// Fetch `output_name`, check its bytes, and store the remote receipt.
///
/// # Errors
///
/// Returns an error when the attempt has no id, the bytes do not match, or the
/// journal names a stale lease.
pub fn record_fetched_receipt(
    transport: &NixplaneCliTransport,
    dir: &Path,
    attempt: &nxr_task::RemoteAttempt,
    output_name: &str,
    journal: &Value,
) -> Result<nxr_task::RemoteReceipt, String> {
    let attempt_id = attempt
        .attempt_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "remote attempt has not been admitted".to_owned())?;
    if let Some(generation) = journal.get("lease_generation").and_then(Value::as_u64) {
        nxr_task::record_lease(dir, &attempt.idempotency_key, generation)?;
    }
    let file = NamedTempFile::new().map_err(|err| format!("temp file: {err}"))?;
    let printed = transport.fetch_output(attempt_id, output_name, file.path())?;
    let bytes = std::fs::read(file.path()).map_err(|err| format!("read artifact: {err}"))?;
    let output_digest = nxr_task::verify_artifact(&bytes, &printed)?;
    let (source_disposition, execution_disposition) = nxr_task::dispositions_from_journal(journal);
    let stored_lease = nxr_task::remote_attempt_for_run(dir, &attempt.run_id)?
        .and_then(|row| row.lease_generation)
        .or(attempt.lease_generation);
    nxr_task::artifact_is_authoritative(journal, stored_lease, &output_digest)?;
    let receipt = nxr_task::RemoteReceipt {
        source_set_id: attempt.source_set_id.clone(),
        operation_id: attempt.operation_id.clone(),
        run_id: attempt.run_id.clone(),
        attempt_id: attempt_id.to_owned(),
        lease_generation: stored_lease,
        output_digest,
        source_disposition,
        execution_disposition,
        artifact_verified: true,
    };
    nxr_task::store_remote_receipt(dir, &receipt)?;
    Ok(receipt)
}

fn replay_argv(attempt_id: &str, after_sequence: u64, state_dir: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "execution".to_owned(),
        "replay".to_owned(),
        attempt_id.to_owned(),
        "--after-sequence".to_owned(),
        after_sequence.to_string(),
    ];
    if let Some(dir) = state_dir {
        argv.push("--state-dir".to_owned());
        argv.push(dir.to_owned());
    }
    argv
}

fn write_json_temp(value: &Value) -> Result<NamedTempFile, String> {
    use std::io::Write;
    let mut file = NamedTempFile::new().map_err(|err| format!("temp file: {err}"))?;
    serde_json::to_writer(&mut file, value).map_err(|err| format!("temp file: {err}"))?;
    file.flush().map_err(|err| format!("temp file: {err}"))?;
    Ok(file)
}

fn dirty_manifest_path(value: &Value) -> Result<(PathBuf, Option<NamedTempFile>), String> {
    match value {
        Value::String(path) => Ok((PathBuf::from(path), None)),
        other => {
            let file = write_json_temp(other)?;
            let path = file.path().to_path_buf();
            Ok((path, Some(file)))
        }
    }
}

fn parse_json_stdout(stdout: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(trimmed).map_err(|err| format!("nixplane stdout: {err}"))
}

fn parse_status_stdout(stdout: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(json!({ "journal": "" }));
    }
    match serde_json::from_str(trimmed) {
        Ok(value) => Ok(value),
        Err(_) => Ok(json!({ "journal": trimmed })),
    }
}

fn run_argv(bin: &str, argv: &[String], status_journal: bool) -> Result<Value, String> {
    let output = Command::new(bin)
        .args(argv)
        .output()
        .map_err(|err| format!("nixplane: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("nixplane exited {}", output.status)
        } else {
            stderr
        });
    }
    if status_journal {
        parse_status_stdout(&output.stdout)
    } else {
        parse_json_stdout(&output.stdout)
    }
}

impl ExecutionTransport for NixplaneCliTransport {
    fn call(&self, method: &str, body: &Value) -> Result<Value, String> {
        let state_dir = self.state_dir.as_deref();
        match method {
            "submit" => {
                let spec = body
                    .get("spec")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| "submit needs spec object".to_owned())?;
                let source_set = body
                    .get("source_set")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| "submit needs source_set object".to_owned())?;
                let spec_file = write_json_temp(spec)?;
                let source_file = write_json_temp(source_set)?;
                let dirty = match body.get("dirty_manifest") {
                    Some(value) if !value.is_null() => Some(dirty_manifest_path(value)?),
                    _ => None,
                };
                let dirty_path = dirty.as_ref().map(|(path, _)| path.as_path());
                let source_locator = body.get("source_locator").and_then(Value::as_str);
                let argv = submit_argv(
                    spec_file.path(),
                    source_file.path(),
                    dirty_path,
                    source_locator,
                    state_dir,
                );
                let result = run_argv(&self.bin, &argv, false);
                drop(dirty);
                drop(source_file);
                drop(spec_file);
                result
            }
            "status" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "status needs attempt_id".to_owned())?;
                run_argv(&self.bin, &status_argv(attempt_id, state_dir), true)
            }
            "cancel" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "cancel needs attempt_id".to_owned())?;
                let value = run_argv(&self.bin, &cancel_argv(attempt_id, state_dir), false)?;
                Ok(normalize_cancel_response(&value))
            }
            "replay" => {
                let attempt_id = body
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "replay needs attempt_id".to_owned())?;
                let after_sequence = body
                    .get("after_sequence")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                run_argv(
                    &self.bin,
                    &replay_argv(attempt_id, after_sequence, state_dir),
                    true,
                )
            }
            other => Err(format!("unknown execution method `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn nixplane_cancel_status_is_acknowledgement() {
        let normalized = normalize_cancel_response(&json!({
            "attempt_id": "att_1",
            "status": "cancelled"
        }));
        assert_eq!(normalized["cancelled"], true);
        assert_eq!(normalized["attempt_id"], "att_1");
        assert_eq!(normalized["status"], "cancelled");
        assert!(normalized.get("process_terminal").is_none());

        let running = normalize_cancel_response(&json!({
            "attempt_id": "att_1",
            "status": "running"
        }));
        assert_eq!(running.get("cancelled").and_then(Value::as_bool), None);
        assert_ne!(running.get("cancelled"), Some(&json!(true)));
    }

    #[test]
    fn submit_argv_includes_optional_flags_without_spawning() {
        let argv = submit_argv(
            Path::new("/tmp/spec.json"),
            Path::new("/tmp/source.json"),
            Some(Path::new("/tmp/dirty.json")),
            Some("git+https://example.invalid/repo"),
            Some("/var/lib/nixplane"),
        );
        assert_eq!(
            argv,
            [
                "execution",
                "submit",
                "/tmp/spec.json",
                "--source-set",
                "/tmp/source.json",
                "--dirty-manifest",
                "/tmp/dirty.json",
                "--source-locator",
                "git+https://example.invalid/repo",
                "--state-dir",
                "/var/lib/nixplane",
            ]
        );
        let minimal = submit_argv(
            Path::new("/tmp/spec.json"),
            Path::new("/tmp/source.json"),
            None,
            None,
            None,
        );
        assert_eq!(
            minimal,
            [
                "execution",
                "submit",
                "/tmp/spec.json",
                "--source-set",
                "/tmp/source.json",
            ]
        );
        assert_eq!(
            fetch_argv(
                "att_1",
                "marker",
                Path::new("/tmp/marker"),
                Some("/var/lib/nixplane"),
            ),
            [
                "execution",
                "fetch",
                "att_1",
                "--name",
                "marker",
                "--out",
                "/tmp/marker",
                "--state-dir",
                "/var/lib/nixplane",
            ]
        );
    }
}
