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
    let pointers = attempt_pointers(&root, name)?;
    let mut follow_id = std::env::var("NIXPLANE_FOLLOW")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if follow_id.is_none() && std::env::var("NIXPLANE_REATTACH").ok().as_deref() == Some("1") {
        if let Some(saved) = saved_receipt(&root, name) {
            match reattach_action(Some(&saved)) {
                "verify" if saved.get("hash_ok").and_then(Value::as_bool) == Some(true) => {
                    let attempt = saved
                        .get("attempt_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let code = saved.get("exit_code").and_then(Value::as_i64).unwrap_or(0) as i32;
                    println!(
                        "reattach verify attempt {attempt} status={}",
                        saved.get("terminal").and_then(Value::as_str).unwrap_or("")
                    );
                    return Ok(code);
                }
                "verify" | "follow" => {
                    follow_id = saved
                        .get("attempt_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    receipt = saved;
                }
                _ => {}
            }
        }
    }
    if let Some(follow) = follow_id {
        receipt["attempt_id"] = json!(follow);
        receipt["submitted"] = json!(true);
        remember_attempts(&pointers, &follow)?;
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
        remember_attempts(&pointers, &attempt)?;
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
    let mut cursor = receipt
        .get("journal_cursor")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut finished: Option<Value> = None;
    if std::env::var("NIXPLANE_REATTACH").ok().as_deref() == Some("1")
        && receipt.get("hash_ok").and_then(Value::as_bool) != Some(true)
        && let Some(terminal) = receipt.get("terminal").and_then(Value::as_str)
        && !terminal.is_empty()
    {
        finished = Some(json!({
            "event_type": terminal,
            "exit_code": receipt.get("exit_code").cloned().unwrap_or(json!(1)),
        }));
    }
    while finished.is_none() {
        if Instant::now() > deadline {
            let mut cancel_args =
                vec!["execution".to_owned(), "cancel".to_owned(), attempt.clone()];
            push_state(&mut cancel_args);
            let _ = nixplane_output(&nixplane, cancel_args);
            receipt["error"] = json!("timed out waiting for a terminal result");
            receipt["journal_cursor"] = json!(cursor);
            receipt["cancelled"] = json!(true);
            write_receipt(&root, &receipt)?;
            clear_attempts(&pointers, &attempt);
            return Ok(124);
        }
        let page = match collect_journal(&nixplane, &attempt, cursor) {
            Ok(page) => page,
            Err(error) => {
                receipt["journal_cursor"] = json!(cursor);
                receipt["error"] = json!(error.to_string());
                write_receipt(&root, &receipt)?;
                return Err(error);
            }
        };
        cursor = journal_cursor(cursor, &page);
        events.extend(page);
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
    receipt["journal_cursor"] = json!(cursor);
    write_receipt(&root, &receipt)?;
    if kind == "cancelled" {
        write_receipt(&root, &receipt)?;
        clear_attempts(&pointers, &attempt);
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
    if let Some(verifier) = output_verifier() {
        run_output_verifier(&verifier, &output_dir, &commit, &mut mismatches)?;
    }
    let hash_ok = mismatches.is_empty();
    receipt["hash_ok"] = json!(hash_ok);
    receipt["journal_cursor"] = json!(cursor);
    write_receipt(&root, &receipt)?;
    clear_attempts(&pointers, &attempt);
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

pub(crate) fn journal_cursor(after: u64, events: &[Value]) -> u64 {
    events
        .iter()
        .rev()
        .find_map(|event| event.get("sequence").and_then(Value::as_u64))
        .map(|sequence| sequence.max(after))
        .unwrap_or(after)
}

fn replay_args(attempt: &str, after: u64) -> Vec<String> {
    vec![
        "execution".to_owned(),
        "replay".to_owned(),
        attempt.to_owned(),
        "--after-sequence".to_owned(),
        after.to_string(),
    ]
}

fn collect_journal(bin: &str, attempt: &str, after: u64) -> Result<Vec<Value>, WorkflowError> {
    let mut events = Vec::new();
    let mut cursor = after;
    loop {
        let mut args = replay_args(attempt, cursor);
        push_state(&mut args);
        let page = nixplane_output(bin, args)?;
        let parsed = parse_journal_page(&page).map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("remote journal after sequence {cursor} was not json: {error}"),
            )
        })?;
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
        if last <= cursor || parsed.len() < 100 {
            return Ok(events);
        }
        cursor = last;
    }
}

fn parse_journal_page(page: &str) -> Result<Vec<Value>, String> {
    let trimmed = page.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(trimmed).map_err(|error| error.to_string())
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

fn output_verifier() -> Option<String> {
    let Ok(command) = std::env::var("NXR_OUTPUT_VERIFY") else {
        return None;
    };
    let command = command.trim();
    if command.is_empty() {
        None
    } else {
        Some(command.to_owned())
    }
}

fn run_output_verifier(
    command: &str,
    dir: &Path,
    commit: &str,
    mismatches: &mut Vec<String>,
) -> Result<(), WorkflowError> {
    let output = Command::new(command)
        .arg(dir)
        .env("NXR_VERIFY_COMMIT", commit)
        .output()
        .map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("output verifier {command}: {error}"),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    mismatches.push(if detail.is_empty() {
        format!(
            "output verifier {command} exited {}",
            output.status.code().unwrap_or(1)
        )
    } else {
        format!("output verifier {command}: {detail}")
    });
    Ok(())
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

fn tool_line(program: &str, args: &[&str]) -> String {
    let Ok(output) = Command::new(program).args(args).output() else {
        return "none".to_owned();
    };
    if !output.status.success() {
        return "none".to_owned();
    }
    String::from_utf8(output.stdout)
        .ok()
        .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "none".to_owned())
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
        "source": commit,
        "rustc": tool_line("rustc", &["--version"]),
        "linker": tool_line("cc", &["--version"]),
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

fn saved_receipt(root: &Path, task: &str) -> Option<Value> {
    let run_id = std::env::var("NXR_RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| run_context().map(|context| context.run_id))?;
    let pointer = leaf_attempt_path(root, &run_id, task);
    let attempt = fs::read_to_string(pointer).ok()?;
    let attempt = attempt.trim();
    if attempt.is_empty() {
        return None;
    }
    let directory = std::env::var("NIXPLANE_RECEIPT_DIR")
        .ok()
        .filter(|text| !text.trim().is_empty())
        .map_or_else(|| root.join(".fabric-receipts"), PathBuf::from);
    let text = fs::read_to_string(directory.join(format!("{attempt}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn attempt_pointers(root: &Path, task: &str) -> Result<Vec<PathBuf>, WorkflowError> {
    let context = run_context();
    let run_id = std::env::var("NXR_RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| context.as_ref().map(|context| context.run_id.clone()))
        .unwrap_or_else(unique_run_id);
    let leaf = leaf_attempt_path(root, &run_id, task);
    let run_dir = leaf
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                "attempt path has no run directory",
            )
        })?;
    fs::create_dir_all(run_dir).map_err(|error| {
        WorkflowError::new(exit::PROCESS_SUPERVISION, format!("run dir: {error}"))
    })?;
    let record = run_dir.join("run.json");
    if !record.exists() {
        let body = json!({"run_id": run_id});
        fs::write(&record, format!("{body}\n")).map_err(|error| {
            WorkflowError::new(exit::PROCESS_SUPERVISION, format!("run record: {error}"))
        })?;
    }
    let mut paths = vec![leaf];
    let leaves = std::env::var("NXR_RUN_LEAVES")
        .ok()
        .and_then(|text| text.parse::<usize>().ok())
        .or_else(|| context.map(|context| context.leaves))
        .unwrap_or(1);
    if leaves <= 1
        && let Ok(shared) = std::env::var("NIXPLANE_ACTIVE_ATTEMPT")
        && !shared.trim().is_empty()
    {
        paths.push(PathBuf::from(shared));
    }
    Ok(paths)
}

/// A new id for every workflow run. Two runs do not share attempt files.
pub(crate) fn unique_run_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("run-{millis}-{}-{sequence}", std::process::id())
}

pub(crate) fn set_run_context(run_id: String, leaves: usize) {
    if let Ok(mut guard) = RUN_CONTEXT.lock() {
        *guard = Some(RunContext { run_id, leaves });
    }
}

fn run_context() -> Option<RunContext> {
    RUN_CONTEXT.lock().ok().and_then(|guard| guard.clone())
}

#[derive(Clone)]
struct RunContext {
    run_id: String,
    leaves: usize,
}

static RUN_CONTEXT: std::sync::Mutex<Option<RunContext>> = std::sync::Mutex::new(None);

fn leaf_attempt_path(root: &Path, run_id: &str, task: &str) -> PathBuf {
    let safe_run = file_token(run_id);
    let safe_task = file_token(task);
    root.join(".nxr")
        .join("runs")
        .join(safe_run)
        .join("tasks")
        .join(safe_task)
        .join("attempt.json")
}

fn file_token(value: &str) -> String {
    let token: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if token.is_empty() {
        "unnamed".to_owned()
    } else {
        token
    }
}

fn remember_attempts(paths: &[PathBuf], attempt: &str) -> Result<(), WorkflowError> {
    for path in paths {
        remember_pointer(path, attempt)?;
    }
    Ok(())
}

fn remember_pointer(path: &Path, attempt: &str) -> Result<(), WorkflowError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            WorkflowError::new(
                exit::PROCESS_SUPERVISION,
                format!("active attempt: {error}"),
            )
        })?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, format!("{attempt}\n")).map_err(|error| {
        WorkflowError::new(
            exit::PROCESS_SUPERVISION,
            format!("active attempt: {error}"),
        )
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        WorkflowError::new(
            exit::PROCESS_SUPERVISION,
            format!("active attempt: {error}"),
        )
    })
}

/// What a stopped run should do with an attempt it already admitted.
///
/// A terminal attempt is checked again. An attempt that has not finished is
/// followed. Neither path submits a second attempt.
pub(crate) fn reattach_action(receipt: Option<&Value>) -> &'static str {
    let Some(receipt) = receipt else {
        return "submit";
    };
    let attempt = receipt
        .get("attempt_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if attempt.is_empty() {
        return "submit";
    }
    if receipt
        .get("terminal")
        .and_then(Value::as_str)
        .is_some_and(|status| !status.is_empty())
    {
        return "verify";
    }
    "follow"
}

fn clear_attempts(paths: &[PathBuf], attempt: &str) {
    for path in paths {
        clear_pointer(path, attempt);
    }
}

fn clear_pointer(path: &Path, attempt: &str) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    if text.trim() == attempt {
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
    fn the_second_poll_requests_the_last_sequence() {
        let first = vec![json!({"sequence": 3}), json!({"sequence": 11})];
        let cursor = journal_cursor(0, &first);
        assert_eq!(cursor, 11);
        let args = replay_args("att_1", cursor);
        let position = args
            .iter()
            .position(|arg| arg == "--after-sequence")
            .expect("cursor flag");
        assert_eq!(args[position + 1], "11");
        assert_eq!(journal_cursor(cursor, &[]), 11);
    }

    #[test]
    fn a_nonzero_verifier_fails_the_transfer_check() {
        let root = tempfile::tempdir().expect("root");
        let script = root.path().join("verify.sh");
        fs::write(&script, "#!/bin/sh\necho mismatch >&2\nexit 7\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let mut mismatches = Vec::new();
        run_output_verifier(
            script.to_str().expect("utf8"),
            root.path(),
            "abc",
            &mut mismatches,
        )
        .expect("spawn");
        assert_eq!(mismatches.len(), 1);
        assert!(mismatches[0].contains("mismatch"), "{}", mismatches[0]);
    }

    #[test]
    fn a_malformed_journal_page_is_not_an_empty_page() {
        assert!(parse_journal_page("").expect("empty").is_empty());
        assert!(parse_journal_page("[]").expect("array").is_empty());
        assert!(parse_journal_page("{").is_err());
        assert!(parse_journal_page("not-json").is_err());
        let page = parse_journal_page(r#"[{"sequence":4}]"#).expect("page");
        assert_eq!(page[0]["sequence"], 4);
    }

    #[test]
    fn finishing_one_leaf_keeps_the_other_attempt_pointer() {
        let root = tempfile::tempdir().expect("root");
        let left = leaf_attempt_path(root.path(), "run-1", "stargate-wasm");
        let right = leaf_attempt_path(root.path(), "run-1", "lattice-check");
        let shared = root.path().join("shared-active");
        remember_pointer(&left, "att_left").expect("left");
        remember_pointer(&right, "att_right").expect("right");
        remember_pointer(&shared, "att_right").expect("shared");
        clear_attempts(&[left.clone(), shared.clone()], "att_left");
        assert!(!left.exists());
        assert_eq!(
            fs::read_to_string(&right).expect("right").trim(),
            "att_right"
        );
        assert_eq!(
            fs::read_to_string(&shared).expect("shared").trim(),
            "att_right"
        );
        clear_attempts(&[right.clone(), shared.clone()], "att_right");
        assert!(!right.exists());
        assert!(!shared.exists());
    }

    #[test]
    fn two_runs_do_not_share_an_attempt_file() {
        let root = tempfile::tempdir().expect("root");
        let first = unique_run_id();
        let second = unique_run_id();
        assert_ne!(first, second);
        let left = leaf_attempt_path(root.path(), &first, "stargate-wasm-bundle");
        let right = leaf_attempt_path(root.path(), &second, "stargate-wasm-bundle");
        remember_pointer(&left, "att_one").expect("one");
        remember_pointer(&right, "att_two").expect("two");
        assert_eq!(fs::read_to_string(&left).expect("left").trim(), "att_one");
        assert_eq!(fs::read_to_string(&right).expect("right").trim(), "att_two");
        assert!(!left.with_extension("tmp").exists());
    }

    #[test]
    fn a_finished_attempt_is_verified_instead_of_submitted() {
        let exited = json!({
            "attempt_id": "att_done",
            "source_set_id": "src_1",
            "artifact_digest": "abc",
            "journal_cursor": 4,
            "terminal": "exited",
            "exit_code": 0
        });
        assert_eq!(reattach_action(Some(&exited)), "verify");
        let interrupted = json!({
            "attempt_id": "att_open",
            "journal_cursor": 2
        });
        assert_eq!(reattach_action(Some(&interrupted)), "follow");
        assert_eq!(reattach_action(None), "submit");
    }
}
