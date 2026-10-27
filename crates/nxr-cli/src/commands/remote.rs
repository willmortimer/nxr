//! `nxr workflow run --execution remote`.
//!
//! Submits one execution spec through the `nixplane` binary, waits for the
//! attempt, fetches each declared output, and writes the receipt. This is not
//! a scheduler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::commands::workflow::WorkflowError;
use nxr_core::diagnostics::exit;

/// Submit one workflow task to an enrolled NixPlane worker.
///
/// # Errors
///
/// Returns [`WorkflowError`] when the spec cannot be built or `nixplane` fails
/// before an attempt exists. A remote task that exits nonzero is `Ok(code)`.
pub fn execute(name: &str) -> Result<i32, WorkflowError> {
    let nixplane = std::env::var("NIXPLANE").unwrap_or_else(|_| "nixplane".to_owned());
    let root = receipt_root()?;
    let (spec_path, source_path, scratch) = prepare_paths(name)?;
    let mut spec: Value = read_json(&spec_path)?;
    if std::env::var("NIXPLANE_PREPARED_SPEC").is_err() {
        apply_rerun(&mut spec)?;
        write_json(&spec_path, &spec)?;
    }
    let source: Value = read_json(&source_path).unwrap_or_else(|_| json!({}));
    let commit = source
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let outputs = output_names(&spec);
    let mut receipt = seed_receipt(&root, name, &spec, &commit);
    if let Ok(follow) = std::env::var("NIXPLANE_FOLLOW")
        && !follow.trim().is_empty()
    {
        receipt["attempt_id"] = json!(follow.trim());
        receipt["submitted"] = json!(true);
    } else {
        let submitted = nixplane_output(&nixplane, submit_args(&spec_path, &source_path))?;
        let accepted: Value = serde_json::from_str(submitted.trim()).map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("submit response was not json: {error}"),
            )
        })?;
        let attempt = accepted
            .get("attempt_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if attempt.is_empty() {
            write_receipt(&root, &receipt)?;
            return Err(WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                "submit did not return an attempt id",
            ));
        }
        receipt["attempt_id"] = json!(attempt);
        receipt["operation_id"] = accepted.get("operation_id").cloned().unwrap_or(json!(null));
        receipt["submit_status"] = accepted.get("status").cloned().unwrap_or(json!(null));
        receipt["submitted"] = json!(true);
        remember_attempt(&attempt)?;
        println!(
            "admitted attempt {attempt} status={}",
            accepted.get("status").and_then(Value::as_str).unwrap_or("")
        );
    }
    let attempt = receipt
        .get("attempt_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let timeout = std::env::var("NIXPLANE_TIMEOUT_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(900);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut events = Vec::new();
    let mut finished: Option<Value> = None;
    while finished.is_none() {
        if Instant::now() > deadline {
            let mut cancel_args =
                vec!["execution".to_owned(), "cancel".to_owned(), attempt.clone()];
            push_state(&mut cancel_args);
            let _ = nixplane_output(&nixplane, cancel_args);
            receipt["error"] = json!("timed out waiting for a terminal result");
            receipt["cancelled"] = json!(true);
            write_receipt(&root, &receipt)?;
            clear_attempt();
            return Ok(124);
        }
        events = collect_journal(&nixplane, &attempt)?;
        absorb_marks(&mut receipt, &events);
        finished = terminal_event(&events);
        if finished.is_none() {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    let finished = finished.unwrap_or(json!({}));
    let code = finished
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or(1) as i32;
    let kind = finished
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("exited");
    receipt["terminal"] = json!(kind);
    receipt["exit_code"] = json!(code);
    if kind == "cancelled" {
        write_receipt(&root, &receipt)?;
        clear_attempt();
        println!("terminal attempt {attempt} cancelled exit={code}");
        drop(scratch);
        return Ok(code);
    }
    let output_dir = root.join(".fabric-receipts").join("outputs").join(&attempt);
    fs::create_dir_all(&output_dir).map_err(|error| {
        WorkflowError::new(exit::PROCESS_SUPERVISION, format!("output dir: {error}"))
    })?;
    let mut resumed = 0_u64;
    let mut fetched = 0_u64;
    let mut mismatches = Vec::new();
    let mut published = serde_json::Map::new();
    for name in &outputs {
        let dest = output_dir.join(name);
        match nixplane_both(
            &nixplane,
            vec![
                "execution".into(),
                "fetch".into(),
                attempt.clone(),
                "--name".into(),
                name.clone(),
                "--out".into(),
                dest.display().to_string(),
            ],
        ) {
            Ok((stdout, stderr)) => {
                let digest = stdout.lines().next_back().unwrap_or("").trim().to_owned();
                if !digest.is_empty() {
                    published.insert(name.clone(), json!(digest));
                }
                let (piece_resumed, piece_fetched) = chunk_counts(&stderr);
                resumed += piece_resumed;
                fetched += piece_fetched;
            }
            Err(error) => mismatches.push(format!("{name} bytes were not fetched: {error}")),
        }
    }
    receipt["output_hashes"] = json!(published);
    receipt["chunks_resumed"] = json!(resumed);
    receipt["chunks_fetched"] = json!(fetched);
    if let Some(object) = receipt.as_object_mut() {
        let timings = object.entry("timings").or_insert_with(|| json!({}));
        if let Some(timings) = timings.as_object_mut() {
            timings.insert("chunks_resumed".to_owned(), json!(resumed));
            timings.insert("chunks_fetched".to_owned(), json!(fetched));
        }
    }
    if let Some(line) = sccache_line(&events) {
        receipt["sccache"] = json!(line);
    }
    verify_desktop(&output_dir, &commit, &mut receipt, &mut mismatches);
    let hash_ok = mismatches.is_empty();
    receipt["hash_ok"] = json!(hash_ok);
    write_receipt(&root, &receipt)?;
    clear_attempt();
    println!("terminal attempt {attempt} exit={code}");
    println!("chunks resumed={resumed} fetched={fetched} hash_ok={hash_ok}");
    for line in &mismatches {
        eprintln!("{line}");
    }
    drop(scratch);
    if !hash_ok {
        return Ok(1);
    }
    Ok(code)
}

fn prepare_paths(
    name: &str,
) -> Result<(PathBuf, PathBuf, Option<tempfile::TempDir>), WorkflowError> {
    if let Ok(spec) = std::env::var("NIXPLANE_PREPARED_SPEC") {
        let source = std::env::var("NIXPLANE_PREPARED_SOURCE").map_err(|_| {
            WorkflowError::new(
                exit::USAGE,
                "NIXPLANE_PREPARED_SOURCE is required with NIXPLANE_PREPARED_SPEC",
            )
        })?;
        return Ok((PathBuf::from(spec), PathBuf::from(source), None));
    }
    let scratch = tempfile::tempdir().map_err(|error| {
        WorkflowError::new(exit::PROCESS_SUPERVISION, format!("temp dir: {error}"))
    })?;
    let source_path = scratch.path().join("source-set.json");
    let spec_path = scratch.path().join("execution-spec.json");
    crate::commands::workflow::write_execution_files(name, &spec_path, &source_path)?;
    Ok((spec_path, source_path, Some(scratch)))
}

fn submit_args(spec: &Path, source: &Path) -> Vec<String> {
    let mut args = vec![
        "execution".to_owned(),
        "submit".to_owned(),
        spec.display().to_string(),
        "--source-set".to_owned(),
        source.display().to_string(),
    ];
    if let Ok(locator) = std::env::var("NIXPLANE_SOURCE_LOCATOR")
        && !locator.trim().is_empty()
    {
        args.push("--source-locator".to_owned());
        args.push(locator);
    }
    if let Ok(manifest) = std::env::var("NIXPLANE_DIRTY_MANIFEST")
        && !manifest.trim().is_empty()
    {
        args.push("--dirty-manifest".to_owned());
        args.push(manifest);
    }
    push_state(&mut args);
    args
}

fn push_state(args: &mut Vec<String>) {
    if let Ok(state) = std::env::var("NIXPLANE_STATE_DIR")
        && !state.trim().is_empty()
    {
        args.push("--state-dir".to_owned());
        args.push(state);
    }
}

fn nixplane_output(bin: &str, args: Vec<String>) -> Result<String, WorkflowError> {
    let (stdout, _) = nixplane_both(bin, args)?;
    Ok(stdout)
}

fn nixplane_both(bin: &str, args: Vec<String>) -> Result<(String, String), WorkflowError> {
    let mut command = Command::new(bin);
    command.args(&args);
    let output = command.output().map_err(|error| {
        WorkflowError::new(
            exit::PROCESS_SUPERVISION,
            format!("failed to spawn {bin}: {error}"),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(WorkflowError::new(
            output.status.code().unwrap_or(exit::PROCESS_SUPERVISION),
            format!("{bin} {}: {stderr}{stdout}", args.join(" ")),
        ));
    }
    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn collect_journal(bin: &str, attempt: &str) -> Result<Vec<Value>, WorkflowError> {
    let mut events = Vec::new();
    let mut after = 0_u64;
    loop {
        let mut args = vec![
            "execution".to_owned(),
            "replay".to_owned(),
            attempt.to_owned(),
            "--after-sequence".to_owned(),
            after.to_string(),
        ];
        push_state(&mut args);
        let page = nixplane_output(bin, args)?;
        let parsed: Vec<Value> = serde_json::from_str(page.trim()).unwrap_or_default();
        if parsed.is_empty() {
            return Ok(events);
        }
        let merged = journal_events(&parsed);
        let last = merged
            .last()
            .and_then(|event| event.get("sequence"))
            .and_then(Value::as_u64);
        events.extend(merged);
        let Some(last) = last else {
            return Ok(events);
        };
        if last <= after || parsed.len() < 100 {
            return Ok(events);
        }
        after = last;
    }
}

fn journal_events(items: &[Value]) -> Vec<Value> {
    let mut events = Vec::new();
    for item in items {
        let body = match item.get("body") {
            Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or(json!({})),
            Some(value) if value.is_object() => value.clone(),
            _ => json!({}),
        };
        let mut merged = match body {
            Value::Object(map) => Value::Object(map),
            _ => json!({}),
        };
        if let (Value::Object(target), Value::Object(source)) = (&mut merged, item) {
            for (key, value) in source {
                if key != "body" {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        events.push(merged);
    }
    events
}

fn terminal_event(events: &[Value]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| {
            matches!(
                event.get("event_type").and_then(Value::as_str),
                Some("exited" | "cancelled")
            )
        })
        .cloned()
}

fn absorb_marks(receipt: &mut Value, events: &[Value]) {
    let mut checkout_ms = None;
    let mut task_ms = None;
    let mut sccache = None;
    for event in events {
        let kind = event
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "source" if checkout_ms.is_none() => checkout_ms = event.get("checkout_ms").cloned(),
            "exited" if task_ms.is_none() => task_ms = event.get("task_ms").cloned(),
            "sccache" if sccache.is_none() => {
                sccache = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(|delta| format!("sccache {delta}"));
            }
            _ => {}
        }
    }
    let Some(object) = receipt.as_object_mut() else {
        return;
    };
    let timings = object
        .entry("timings")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(timings) = timings {
        if let Some(value) = checkout_ms.clone() {
            timings.entry("checkout_ms").or_insert(value);
        }
        if let Some(value) = task_ms.clone() {
            timings.entry("task_ms").or_insert(value);
        }
    }
    if let Some(value) = checkout_ms {
        object.entry("checkout_ms").or_insert(value);
    }
    if let Some(value) = task_ms {
        object.entry("task_ms").or_insert(value);
    }
    if let Some(line) = sccache {
        object.entry("sccache").or_insert(json!(line));
    }
}

fn sccache_line(events: &[Value]) -> Option<String> {
    events.iter().rev().find_map(|event| {
        if event.get("event_type").and_then(Value::as_str) != Some("sccache") {
            return None;
        }
        event
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| format!("sccache {delta}"))
    })
}

fn verify_desktop(dir: &Path, commit: &str, receipt: &mut Value, mismatches: &mut Vec<String>) {
    let manifest_path = dir.join("lattice-desktop-manifest.txt");
    let Ok(text) = fs::read_to_string(&manifest_path) else {
        return;
    };
    if !commit.is_empty() && !text.starts_with(&format!("lattice {commit}\n")) {
        mismatches.push("desktop manifest does not name the submitted commit".to_owned());
    }
    receipt["desktop_manifest"] = json!(text);
    if let Some(toolchain) = field(&text, "rustc ") {
        receipt["toolchain"] = json!(toolchain);
    }
    if let Some(linker) = field(&text, "linker ") {
        receipt["linker"] = json!(linker);
    }
    if receipt.get("sccache").is_none()
        && let Some(line) = text.lines().find(|line| line.starts_with("sccache "))
    {
        receipt["sccache"] = json!(line);
    }
    let Some(expected) = field(&text, "sha256 ") else {
        mismatches.push("desktop manifest is missing the binary hash".to_owned());
        return;
    };
    let binary = ["lattice-desktop.exe", "lattice-desktop"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file());
    let Some(binary) = binary else {
        mismatches.push("desktop binary was not fetched".to_owned());
        return;
    };
    match file_sha256(&binary) {
        Ok(actual) if actual == expected => {
            receipt["verified_sha256"] = json!(actual);
        }
        Ok(actual) => mismatches.push(format!(
            "retrieved desktop bytes {actual} do not match the manifest hash {expected}"
        )),
        Err(error) => mismatches.push(error),
    }
}

fn field(text: &str, prefix: &str) -> Option<String> {
    text.lines()
        .find(|line| line.starts_with(prefix))
        .map(|line| line[prefix.len()..].trim().to_owned())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    for (program, extra) in [("sha256sum", &[] as &[&str]), ("shasum", &["-a", "256"])] {
        let mut command = Command::new(program);
        command.args(extra).arg(path);
        let Ok(output) = command.output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let digest = text.split_whitespace().next().unwrap_or("");
        if digest.len() == 64 {
            return Ok(digest.to_owned());
        }
    }
    Err(format!("could not hash {}", path.display()))
}

pub(crate) fn chunk_counts(stderr: &str) -> (u64, u64) {
    let mut resumed = 0;
    let mut fetched = 0;
    for line in stderr.lines() {
        let Some(rest) = line.trim().strip_prefix("chunks ") else {
            continue;
        };
        for part in rest.split_whitespace() {
            if let Some(value) = part.strip_prefix("resumed=") {
                resumed += value.parse::<u64>().unwrap_or(0);
            }
            if let Some(value) = part.strip_prefix("fetched=") {
                fetched += value.parse::<u64>().unwrap_or(0);
            }
        }
    }
    (resumed, fetched)
}

fn output_names(spec: &Value) -> Vec<String> {
    spec.get("outputs")
        .and_then(Value::as_array)
        .map(|outputs| {
            outputs
                .iter()
                .filter_map(|output| {
                    output
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn seed_receipt(root: &Path, name: &str, spec: &Value, commit: &str) -> Value {
    if let Ok(path) = std::env::var("NIXPLANE_RECEIPT_SEED")
        && let Ok(text) = fs::read_to_string(path)
        && let Ok(value) = serde_json::from_str::<Value>(&text)
    {
        return value;
    }
    json!({
        "task_id": name,
        "source_set_id": spec.get("source_set_id").and_then(Value::as_str).unwrap_or(""),
        "operation_id": spec.get("operation_id").and_then(Value::as_str).unwrap_or(""),
        "commit": commit,
        "lattice_revision": commit,
        "submitted": false,
        "timings": {},
        "receipt_root": root.display().to_string(),
    })
}

fn apply_rerun(spec: &mut Value) -> Result<(), WorkflowError> {
    let Ok(rerun) = std::env::var("NIXPLANE_RERUN") else {
        return Ok(());
    };
    let rerun = rerun.trim();
    if rerun.is_empty() {
        return Ok(());
    }
    if !rerun
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-')
        || rerun.len() > 40
    {
        return Err(WorkflowError::new(
            exit::USAGE,
            "NIXPLANE_RERUN must be a short token",
        ));
    }
    let key = spec
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    spec["idempotency_key"] = json!(format!("{key}-{rerun}"));
    Ok(())
}

fn receipt_root() -> Result<PathBuf, WorkflowError> {
    if let Ok(root) = std::env::var("NIXPLANE_RECEIPT_ROOT")
        && !root.trim().is_empty()
    {
        return Ok(PathBuf::from(root));
    }
    std::env::current_dir().map_err(|error| WorkflowError::new(exit::DISCOVERY, error.to_string()))
}

fn write_receipt(root: &Path, receipt: &Value) -> Result<(), WorkflowError> {
    let directory = std::env::var("NIXPLANE_RECEIPT_DIR")
        .ok()
        .filter(|text| !text.trim().is_empty())
        .map_or_else(|| root.join(".fabric-receipts"), PathBuf::from);
    fs::create_dir_all(&directory).map_err(|error| {
        WorkflowError::new(exit::PROCESS_SUPERVISION, format!("receipt dir: {error}"))
    })?;
    let attempt = receipt
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("unsubmitted");
    let path = directory.join(format!("{attempt}.json"));
    let text = serde_json::to_string_pretty(receipt)
        .map_err(|error| WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string()))?;
    fs::write(&path, format!("{text}\n")).map_err(|error| {
        WorkflowError::new(exit::PROCESS_SUPERVISION, format!("write receipt: {error}"))
    })?;
    if let Ok(result) = std::env::var("NIXPLANE_RESULT") {
        let _ = fs::write(
            result,
            format!(
                "{}\n",
                json!({
                    "attempt_id": attempt,
                    "receipt": path.display().to_string(),
                    "exit_code": receipt.get("exit_code").cloned().unwrap_or(json!(null)),
                    "hash_ok": receipt.get("hash_ok").cloned().unwrap_or(json!(null)),
                    "output_dir": directory.join("outputs").join(attempt).display().to_string(),
                })
            ),
        );
    }
    println!("receipt {}", path.display());
    Ok(())
}

fn remember_attempt(attempt: &str) -> Result<(), WorkflowError> {
    if let Ok(path) = std::env::var("NIXPLANE_ACTIVE_ATTEMPT") {
        if let Some(parent) = Path::new(&path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, attempt).map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("active attempt: {error}"),
            )
        })?;
    }
    Ok(())
}

fn clear_attempt() {
    if let Ok(path) = std::env::var("NIXPLANE_ACTIVE_ATTEMPT") {
        let _ = fs::remove_file(path);
    }
}

fn read_json(path: &Path) -> Result<Value, WorkflowError> {
    let text = fs::read_to_string(path).map_err(|error| {
        WorkflowError::new(exit::USAGE, format!("read {}: {error}", path.display()))
    })?;
    serde_json::from_str(&text).map_err(|error| {
        WorkflowError::new(exit::USAGE, format!("json {}: {error}", path.display()))
    })
}

fn write_json(path: &Path, value: &Value) -> Result<(), WorkflowError> {
    let text = serde_json::to_string(value)
        .map_err(|error| WorkflowError::new(exit::PROCESS_SUPERVISION, error.to_string()))?;
    fs::write(path, text).map_err(|error| {
        WorkflowError::new(
            exit::PROCESS_SUPERVISION,
            format!("write {}: {error}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_resume_counts_add_across_fetches() {
        let (resumed, fetched) = chunk_counts("chunks resumed=4 fetched=4\n");
        assert_eq!((resumed, fetched), (4, 4));
        let (again, more) = chunk_counts("noise\nchunks resumed=0 fetched=2\n");
        assert_eq!((again, more), (0, 2));
    }

    #[test]
    fn a_manifest_names_the_commit_toolchain_and_linker() {
        let text = "lattice abc\nrustc 1.98.0\ntarget x86_64-pc-windows-msvc\nsha256 d6c1a8f7\nlinker link.exe\nsccache stock hits=90 misses=0 non-cacheable=11\n";
        assert_eq!(field(text, "rustc ").as_deref(), Some("1.98.0"));
        assert_eq!(field(text, "linker ").as_deref(), Some("link.exe"));
        assert_eq!(field(text, "sha256 ").as_deref(), Some("d6c1a8f7"));
        assert!(text.starts_with("lattice abc\n"));
    }
}
