//! Verified artifact materialization for workflow inputs.
//!
//! A record names the producer, the content digest, and the consumer input.
//! Materialization hashes every file the manifest lists before it replaces a
//! destination. A failed check leaves the previous destination in place.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;

use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::execute::ExecContext;

const UNPACK_LIMIT: u64 = 64 * 1024 * 1024;
const ENTRY_LIMIT: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub name: String,
    pub identity: Value,
    pub content_digest: String,
    pub producer_run: String,
    pub host: String,
    pub physical_host: String,
    pub facet: String,
    pub consumer_input: String,
    pub attempt_id: String,
    pub output: String,
}

impl ArtifactRecord {
    fn from_value(value: &Value) -> Result<Self, String> {
        let content_digest = value
            .get("contentDigest")
            .or_else(|| value.get("bundleSha256"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !is_hex_digest(&content_digest) {
            return Err("artifact record has no content digest".into());
        }
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        safe_segment(&name, "artifact name")?;
        let output = value
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("bundle.tar")
            .to_owned();
        safe_segment(&output, "artifact output")?;
        Ok(Self {
            name,
            identity: value.get("identity").cloned().unwrap_or(json!({})),
            content_digest,
            producer_run: value
                .get("producerRun")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            host: value
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            physical_host: value
                .get("physicalHost")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            facet: value
                .get("facet")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            consumer_input: value
                .get("consumerInput")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            attempt_id: value
                .get("attemptId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            output,
        })
    }
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn safe_segment(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 200
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.contains('\0')
        || value == "."
    {
        return Err(format!("{label} is not a single path segment"));
    }
    Ok(())
}

pub fn local_host() -> &'static str {
    std::env::consts::OS
}

/// Materialize each named input before the leaf process starts.
pub fn materialize_inputs(ctx: &ExecContext, inputs: &[String]) -> Result<(), String> {
    materialize_inputs_from(ctx, inputs, catalog_root().as_deref())
}

fn materialize_inputs_from(
    ctx: &ExecContext,
    inputs: &[String],
    catalog: Option<&Path>,
) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let root = ctx
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().map_err(|err| err.to_string())?);
    let store = root.join(".nxr").join("artifacts");
    for input in inputs {
        safe_segment(input, "artifact input")?;
        let record = load_record(&store, input)?;
        let destination = store.join("materialized").join(input);
        let checkout_object = store.join("objects").join(&record.content_digest);
        let changed = catalog
            .and_then(|root| changed_identity_field(root, &record))
            .unwrap_or_default();
        if let Some(catalog) = catalog
            && catalog_object_ok(catalog, &record)
        {
            let digest = record.content_digest.clone();
            with_exclusive_lock(&object_lock_path(catalog, &digest), || {
                let source = catalog.join("objects").join(&digest);
                publish_tree(&source, &checkout_object, &digest)
            })?;
            restore_local(&store, &record, &destination)?;
            write_materialize_decision(&store, input, &record, "local", "hit", &changed)?;
            continue;
        }
        if verify_tree(&checkout_object, &record.content_digest).is_ok() {
            if let Some(catalog) = catalog {
                accept_catalog_publish(publish_record_tree(catalog, &record, &checkout_object))?;
            }
            restore_local(&store, &record, &destination)?;
            write_materialize_decision(&store, input, &record, "local", "hit", &changed)?;
            continue;
        }
        let corrupt = checkout_object.join("manifest.json").is_file();
        if !record.attempt_id.is_empty() && (corrupt || !checkout_object.exists()) {
            fetch_remote(&record, &destination)?;
            if verify_tree(&destination, &record.content_digest).is_ok() {
                publish_tree(&destination, &checkout_object, &record.content_digest)?;
                if let Some(catalog) = catalog {
                    accept_catalog_publish(publish_record_tree(catalog, &record, &destination))?;
                }
            }
            write_materialize_decision(&store, input, &record, "fetch", "miss", &changed)?;
            continue;
        }
        if corrupt {
            verify_tree(&checkout_object, &record.content_digest)?;
        }
        return Err(format!(
            "artifact {input} is not available locally and has no attempt id"
        ));
    }
    Ok(())
}

/// Copy verified checkout records into the host-local catalog.
///
/// The catalog key is the canonical input digest and the task name.
/// `physicalHost` and `facet` stay labels on the record. A record with no
/// source identity is skipped. The same inputs with a different content
/// digest are a conflict and leave the stored object in place.
pub fn publish_checkout_catalog(root: &Path) -> Result<(), String> {
    let Some(catalog) = catalog_root() else {
        return Ok(());
    };
    let store = root.join(".nxr").join("artifacts");
    let records = store.join("records");
    if !records.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&records).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Ok(record) = ArtifactRecord::from_value(&value) else {
            continue;
        };
        let object = store.join("objects").join(&record.content_digest);
        if verify_tree(&object, &record.content_digest).is_ok() {
            match publish_record_tree(&catalog, &record, &object) {
                Ok(()) => {}
                Err(err) if err.contains("no source identity") => continue,
                Err(err) => return Err(err),
            }
        }
    }
    Ok(())
}

fn catalog_root() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("nxr").join("artifacts"))
}

fn accept_catalog_publish(result: Result<(), String>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(err) if err.contains("no source identity") || err.contains("catalog conflict") => {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Directory name for a reusable record: sha256 of the task inputs.
///
/// Stargate commit, profile, toolchain, lockfile, and dirty-source digest
/// form the key when they are present. A repository SourceSet stays on the
/// record as provenance and does not change that key. A generic record uses
/// its source-set id, toolchain, and inputs. The result is hex, so the
/// directory does not depend on path separators.
fn catalog_key(record: &ArtifactRecord) -> Result<String, String> {
    let lines = stargate_input_lines(&record.identity)
        .or_else(|| generic_input_lines(&record.identity))
        .ok_or_else(|| "artifact record has no source identity".to_owned())?;
    Ok(hex_digest(lines.join("\n").as_bytes()))
}

fn stargate_input_lines(identity: &Value) -> Option<Vec<String>> {
    let commit = text_at(identity, "commit")?;
    let profile = text_at(identity, "profile")?;
    let toolchain = identity.get("toolchain")?.as_object()?;
    let rustc = object_text(toolchain, "rustc")?;
    let wasm_bindgen = object_text(toolchain, "wasmBindgen")?;
    let binaryen = object_text(toolchain, "binaryen")?;
    let lockfile = first_required(identity, &["lockfile", "lockfileDigest", "lockfile_digest"])?;
    let dirty = first_required(
        identity,
        &["dirty", "dirtySource", "dirty_snapshot_id", "dirtySnapshot"],
    )?;
    Some(vec![
        "stargate".to_owned(),
        commit,
        profile,
        rustc,
        wasm_bindgen,
        binaryen,
        lockfile,
        dirty,
    ])
}

fn generic_input_lines(identity: &Value) -> Option<Vec<String>> {
    let source = ["sourceSet", "source_set", "source_set_id"]
        .iter()
        .find_map(|key| text_at(identity, key))?;
    let toolchain = identity
        .get("toolchain")
        .map(canonical_fragment)
        .filter(|value| !value.is_empty())?;
    let inputs = identity
        .get("inputs")
        .or_else(|| identity.get("inputDigests"))
        .map(canonical_fragment)
        .filter(|value| !value.is_empty())?;
    Some(vec!["source-set".to_owned(), source, toolchain, inputs])
}

fn object_text(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn first_required(identity: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| text_at(identity, key))
}

fn text_at(identity: &Value, key: &str) -> Option<String> {
    identity
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn canonical_fragment(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            keys.into_iter()
                .map(|key| format!("{key}={}", canonical_fragment(&map[&key])))
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Array(items) => items
            .iter()
            .map(canonical_fragment)
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn legacy_record_dirs(record: &ArtifactRecord) -> Vec<String> {
    let mut dirs = vec!["unscoped".to_owned()];
    for field in ["sourceSet", "source_set", "source_set_id"] {
        if let Some(value) = text_at(&record.identity, field)
            && value != "unscoped"
            && safe_segment(&value, "source set").is_ok()
        {
            dirs.push(value);
        }
    }
    dirs
}

fn stored_content_digest(catalog: &Path, dir: &str, name: &str) -> Option<String> {
    let path = catalog
        .join("records")
        .join(dir)
        .join(format!("{name}.json"));
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let stored = ArtifactRecord::from_value(&value).ok()?;
    Some(stored.content_digest)
}

fn object_verified(catalog: &Path, digest: &str) -> bool {
    verify_tree(&catalog.join("objects").join(digest), digest).is_ok()
}

fn catalog_object_ok(catalog: &Path, record: &ArtifactRecord) -> bool {
    let Ok(key) = catalog_key(record) else {
        return false;
    };
    match stored_content_digest(catalog, &key, &record.name) {
        Some(digest) if digest == record.content_digest => {
            return object_verified(catalog, &record.content_digest);
        }
        Some(_) => return false,
        None => {}
    }
    legacy_record_dirs(record).into_iter().any(|dir| {
        stored_content_digest(catalog, &dir, &record.name).as_deref()
            == Some(record.content_digest.as_str())
            && object_verified(catalog, &record.content_digest)
    })
}

fn publish_record_tree(
    catalog: &Path,
    record: &ArtifactRecord,
    object: &Path,
) -> Result<(), String> {
    let record = prepare_publication(record)?;
    let key = catalog_key(&record)?;
    let record_dir = catalog.join("records").join(&key);
    fs::create_dir_all(&record_dir).map_err(|err| err.to_string())?;
    let lock_path = record_dir.join(format!("{}.lock", record.name));
    with_exclusive_lock(&lock_path, || {
        let record_path = record_dir.join(format!("{}.json", record.name));
        if let Some(stored) = stored_content_digest(catalog, &key, &record.name)
            && stored != record.content_digest
        {
            return Err(format!(
                "catalog conflict for {} at {key}: stored {stored} does not match {}",
                record.name, record.content_digest
            ));
        }
        let destination = catalog.join("objects").join(&record.content_digest);
        publish_tree(object, &destination, &record.content_digest)?;
        let body = json!({
            "attemptId": record.attempt_id,
            "consumerInput": record.consumer_input,
            "contentDigest": record.content_digest,
            "facet": record.facet,
            "host": record.host,
            "identity": record.identity,
            "name": record.name,
            "output": record.output,
            "physicalHost": record.physical_host,
            "producerRun": record.producer_run,
        });
        atomic_replace(&record_path, &format!("{body}\n"))
    })
}

fn with_exclusive_lock<T>(
    lock_path: &Path,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|err| err.to_string())?;
    file.lock_exclusive().map_err(|err| err.to_string())?;
    let result = body();
    let _ = file.unlock();
    result
}

fn atomic_replace(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("record");
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&temporary, body).map_err(|err| err.to_string())?;
    fs::rename(&temporary, path).map_err(|err| {
        let _ = fs::remove_file(&temporary);
        err.to_string()
    })
}

fn prepare_publication(record: &ArtifactRecord) -> Result<ArtifactRecord, String> {
    reject_unknown_producer(&record.identity)?;
    reject_noncanonical_generic_inputs(&record.identity)?;
    let mut record = record.clone();
    record.identity = identity_with_schema(&record.identity);
    Ok(record)
}

fn reject_unknown_producer(identity: &Value) -> Result<(), String> {
    match identity.get("producer").and_then(Value::as_str) {
        None | Some("stargate") | Some("generic") => Ok(()),
        Some(kind) => Err(format!("unknown producer kind: {kind}")),
    }
}

fn reject_noncanonical_generic_inputs(identity: &Value) -> Result<(), String> {
    let kind = identity.get("producer").and_then(Value::as_str);
    let generic = kind == Some("generic")
        || (kind.is_none()
            && (identity.get("inputs").is_some() || identity.get("inputDigests").is_some()));
    if !generic {
        return Ok(());
    }
    for key in ["inputs", "inputDigests"] {
        let Some(value) = identity.get(key) else {
            continue;
        };
        if value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .is_none()
        {
            return Err("generic artifact inputs are not canonical".into());
        }
    }
    Ok(())
}

fn identity_with_schema(identity: &Value) -> Value {
    let mut value = identity.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("identitySchema".to_owned(), json!(1));
    }
    sort_json(&value)
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&map[&key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}

fn changed_identity_field(catalog: &Path, current: &ArtifactRecord) -> Option<String> {
    let records = catalog.join("records");
    let mut directories = fs::read_dir(&records)
        .ok()?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    directories.sort_by_key(|entry| entry.file_name());
    for source_set in directories {
        let path = source_set.path().join(format!("{}.json", current.name));
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Ok(stored) = ArtifactRecord::from_value(&value) else {
            continue;
        };
        if let Some(field) = first_changed_field(&current.identity, &stored.identity) {
            return Some(field);
        }
    }
    None
}

fn first_changed_field(current: &Value, stored: &Value) -> Option<String> {
    for (label, keys) in [
        ("commit", &["commit"][..]),
        ("profile", &["profile"][..]),
        ("toolchain", &["toolchain"][..]),
        (
            "lockfile",
            &["lockfile", "lockfileDigest", "lockfile_digest"][..],
        ),
        (
            "dirty",
            &["dirty", "dirtySource", "dirty_snapshot_id", "dirtySnapshot"][..],
        ),
        (
            "sourceSet",
            &["sourceSet", "source_set", "source_set_id"][..],
        ),
        ("inputs", &["inputs", "inputDigests"][..]),
    ] {
        let left = identity_value(current, keys);
        let right = identity_value(stored, keys);
        if left != right && (left.is_some() || right.is_some()) {
            return Some(label.to_owned());
        }
    }
    None
}

fn identity_value<'a>(identity: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| identity.get(*key))
}

/// Copy run timings onto the stored materialize decision.
///
/// A local hit does not copy `task_ms` into `savings_ms`. Savings are
/// `measured` against the median rebuild of the same task, toolchain, and
/// input digest, `estimated` against the same task and toolchain, or
/// `unavailable` when no comparable sample exists.
pub fn annotate_decision(
    root: &Path,
    task: &str,
    task_ms: Option<&Value>,
    checkout_ms: Option<&Value>,
    chunks_resumed: u64,
    chunks_fetched: u64,
) -> Result<(), String> {
    annotate_decision_from(
        root,
        task,
        task_ms,
        checkout_ms,
        chunks_resumed,
        chunks_fetched,
        catalog_root().as_deref(),
    )
}

fn annotate_decision_from(
    root: &Path,
    task: &str,
    task_ms: Option<&Value>,
    checkout_ms: Option<&Value>,
    chunks_resumed: u64,
    chunks_fetched: u64,
    catalog: Option<&Path>,
) -> Result<(), String> {
    let path = root
        .join(".nxr")
        .join("artifacts")
        .join("decisions")
        .join(format!("{task}.json"));
    if !path.is_file() {
        return Ok(());
    }
    let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let mut decision: Value =
        serde_json::from_str(&text).map_err(|err| format!("decision json: {err}"))?;
    let Some(object) = decision.as_object_mut() else {
        return Ok(());
    };
    if let Some(value) = task_ms {
        object.insert("task_ms".to_owned(), value.clone());
    }
    if let Some(value) = checkout_ms {
        object.insert("checkout_ms".to_owned(), value.clone());
    }
    object.insert("chunks_resumed".to_owned(), json!(chunks_resumed));
    object.insert("chunks_fetched".to_owned(), json!(chunks_fetched));
    let decision_kind = object
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let transfer = object
        .get("transfer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let record = load_checkout_record(root, task);
    let observed = task_ms.and_then(json_u64);
    if decision_kind == "rebuild" {
        if let (Some(catalog), Some(record), Some(elapsed)) = (catalog, record.as_ref(), observed) {
            record_rebuild_sample(catalog, task, record, elapsed)?;
        }
    }
    let (basis, savings) = if transfer == "local" && decision_kind == "hit" {
        match (catalog, record.as_ref(), observed) {
            (Some(catalog), Some(record), Some(elapsed)) => {
                savings_against_baseline(catalog, task, record, elapsed)
            }
            _ => ("unavailable".to_owned(), None),
        }
    } else {
        ("unavailable".to_owned(), None)
    };
    object.insert("savingsBasis".to_owned(), json!(basis));
    match savings {
        Some(value) => {
            object.insert("savings_ms".to_owned(), json!(value));
        }
        None => {
            object.remove("savings_ms");
        }
    }
    fs::write(&path, format!("{decision}\n")).map_err(|err| err.to_string())
}

fn load_checkout_record(root: &Path, task: &str) -> Option<ArtifactRecord> {
    let path = root
        .join(".nxr")
        .join("artifacts")
        .join("records")
        .join(format!("{task}.json"));
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    ArtifactRecord::from_value(&value).ok()
}

fn toolchain_digest(record: &ArtifactRecord) -> Result<String, String> {
    let fragment = record
        .identity
        .get("toolchain")
        .map(canonical_fragment)
        .unwrap_or_default();
    Ok(hex_digest(fragment.as_bytes()))
}

const BASELINE_SAMPLE_CAP: usize = 16;

fn record_rebuild_sample(
    catalog: &Path,
    task: &str,
    record: &ArtifactRecord,
    task_ms: u64,
) -> Result<(), String> {
    if safe_segment(task, "artifact task").is_err() {
        return Ok(());
    }
    let input = catalog_key(record)?;
    let toolchain = toolchain_digest(record)?;
    let directory = catalog.join("baselines").join(task);
    fs::create_dir_all(&directory).map_err(|err| err.to_string())?;
    let path = directory.join(format!("{toolchain}.json"));
    let lock_path = directory.join(format!("{toolchain}.lock"));
    with_exclusive_lock(&lock_path, || {
        let mut samples = read_baseline_samples(&path);
        samples.push(json!({ "input": input, "task_ms": task_ms }));
        if samples.len() > BASELINE_SAMPLE_CAP {
            let extra = samples.len() - BASELINE_SAMPLE_CAP;
            samples.drain(0..extra);
        }
        let body = json!({ "samples": samples });
        atomic_replace(&path, &format!("{body}\n"))
    })
}

fn read_baseline_samples(path: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("samples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn savings_against_baseline(
    catalog: &Path,
    task: &str,
    record: &ArtifactRecord,
    task_ms: u64,
) -> (String, Option<i64>) {
    let Ok(input) = catalog_key(record) else {
        return ("unavailable".to_owned(), None);
    };
    let Ok(toolchain) = toolchain_digest(record) else {
        return ("unavailable".to_owned(), None);
    };
    let path = catalog
        .join("baselines")
        .join(task)
        .join(format!("{toolchain}.json"));
    let samples = read_baseline_samples(&path);
    let exact: Vec<u64> = samples
        .iter()
        .filter(|sample| sample.get("input").and_then(Value::as_str) == Some(input.as_str()))
        .filter_map(|sample| sample.get("task_ms").and_then(json_u64))
        .collect();
    if let Some(median) = median_ms(&exact) {
        return ("measured".to_owned(), Some(signed_delta(median, task_ms)));
    }
    let class: Vec<u64> = samples
        .iter()
        .filter_map(|sample| sample.get("task_ms").and_then(json_u64))
        .collect();
    if let Some(median) = median_ms(&class) {
        return ("estimated".to_owned(), Some(signed_delta(median, task_ms)));
    }
    ("unavailable".to_owned(), None)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
}

fn median_ms(samples: &[u64]) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some(sorted[mid - 1].saturating_add(sorted[mid]) / 2)
    }
}

fn signed_delta(baseline: u64, observed: u64) -> i64 {
    i64::try_from(baseline).unwrap_or(i64::MAX) - i64::try_from(observed).unwrap_or(0)
}

fn write_materialize_decision(
    store: &Path,
    input: &str,
    record: &ArtifactRecord,
    transfer: &str,
    decision: &str,
    changed: &str,
) -> Result<(), String> {
    let path = store.join("decisions").join(format!("{input}.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let identity_fields = record
        .identity
        .as_object()
        .map(|fields| fields.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    let body = json!({
        "artifact": decision,
        "cargo": recorded_tool(store, input, "cargo"),
        "changedField": changed,
        "chunks_fetched": 0,
        "chunks_resumed": 0,
        "decision": decision,
        "digest": record.content_digest,
        "facet": record.facet,
        "identityFields": identity_fields,
        "physicalHost": record.physical_host,
        "reason": changed,
        "savingsBasis": "unavailable",
        "sccache": recorded_tool(store, input, "sccache"),
        "task": input,
        "transfer": transfer,
    });
    fs::write(path, format!("{body}\n")).map_err(|err| err.to_string())
}

fn recorded_tool(store: &Path, input: &str, tool: &str) -> &'static str {
    let path = store.join("receipts").join(format!("{input}.json"));
    let Ok(text) = fs::read_to_string(path) else {
        return "not-recorded";
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return "not-recorded";
    };
    if value.get(tool).and_then(Value::as_str) == Some("reused") {
        "reused"
    } else {
        "not-recorded"
    }
}

/// Drop unreferenced records past `quota`, quarantine a record whose object
/// does not verify, and leave an object in place while its materialize lock is held.
pub fn retain_catalog(catalog: &Path, quota: usize) -> Result<(), String> {
    quarantine_unverified(catalog)?;
    let mut records = record_files(catalog)?;
    records.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then(left.path.cmp(&right.path))
    });
    let pinned = pinned_digests(catalog);
    let mut remaining = records.len();
    for record in records {
        if remaining <= quota {
            break;
        }
        if pinned.contains(&record.digest) {
            continue;
        }
        fs::remove_file(&record.path).map_err(|err| err.to_string())?;
        remaining -= 1;
        if !digest_still_recorded(catalog, &record.digest) {
            let _ = delete_object_if_unlocked(catalog, &record.digest)?;
        }
    }
    delete_orphan_objects(catalog)
}

fn delete_orphan_objects(catalog: &Path) -> Result<(), String> {
    let objects = catalog.join("objects");
    if !objects.is_dir() {
        return Ok(());
    }
    let recorded = record_files(catalog)?;
    for entry in fs::read_dir(&objects).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        if !entry.file_type().map_err(|err| err.to_string())?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if recorded.iter().any(|record| record.digest == name) {
            continue;
        }
        delete_object_if_unlocked(catalog, &name)?;
    }
    Ok(())
}

struct CatalogRecordFile {
    path: std::path::PathBuf,
    digest: String,
    modified: std::time::SystemTime,
}

fn record_files(catalog: &Path) -> Result<Vec<CatalogRecordFile>, String> {
    let records = catalog.join("records");
    if !records.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for directory in fs::read_dir(&records).map_err(|err| err.to_string())? {
        let directory = directory.map_err(|err| err.to_string())?;
        if !directory
            .file_type()
            .map_err(|err| err.to_string())?
            .is_dir()
        {
            continue;
        }
        for entry in fs::read_dir(directory.path()).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            let value: Value =
                serde_json::from_str(&text).map_err(|err| format!("catalog record: {err}"))?;
            let record = ArtifactRecord::from_value(&value)?;
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files.push(CatalogRecordFile {
                path,
                digest: record.content_digest,
                modified,
            });
        }
    }
    Ok(files)
}

fn quarantine_unverified(catalog: &Path) -> Result<(), String> {
    let records = record_files(catalog)?;
    for record in records {
        let object = catalog.join("objects").join(&record.digest);
        if verify_tree(&object, &record.digest).is_ok() {
            continue;
        }
        let relative = record
            .path
            .strip_prefix(catalog.join("records"))
            .unwrap_or(record.path.as_path());
        let destination = catalog.join("quarantine").join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::rename(&record.path, &destination).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn pinned_digests(catalog: &Path) -> std::collections::BTreeSet<String> {
    let mut digests = std::collections::BTreeSet::new();
    let pins = catalog.join("pins");
    let Ok(entries) = fs::read_dir(&pins) else {
        return digests;
    };
    for entry in entries.filter_map(Result::ok) {
        if let Some(name) = entry.file_name().to_str() {
            digests.insert(name.to_owned());
        }
    }
    digests
}

fn digest_still_recorded(catalog: &Path, digest: &str) -> bool {
    record_files(catalog)
        .ok()
        .is_some_and(|records| records.iter().any(|record| record.digest == digest))
}

fn object_lock_path(catalog: &Path, digest: &str) -> std::path::PathBuf {
    catalog.join("locks").join(format!("{digest}.lock"))
}

fn delete_object_if_unlocked(catalog: &Path, digest: &str) -> Result<bool, String> {
    let lock_path = object_lock_path(catalog, digest);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|err| err.to_string())?;
    if file.try_lock_exclusive().is_err() {
        return Ok(false);
    }
    let object = catalog.join("objects").join(digest);
    if object.exists() {
        fs::remove_dir_all(&object).map_err(|err| err.to_string())?;
    }
    let _ = file.unlock();
    Ok(true)
}

fn load_record(store: &Path, name: &str) -> Result<ArtifactRecord, String> {
    let path = store.join("records").join(format!("{name}.json"));
    let text = fs::read_to_string(&path)
        .map_err(|_| format!("no stored artifact for {name} at {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|err| format!("artifact record for {name} is not json: {err}"))?;
    ArtifactRecord::from_value(&value)
}

fn restore_local(store: &Path, record: &ArtifactRecord, destination: &Path) -> Result<(), String> {
    let source = store.join("objects").join(&record.content_digest);
    verify_tree(&source, &record.content_digest)?;
    if destination.exists() && verify_tree(destination, &record.content_digest).is_ok() {
        return Ok(());
    }
    publish_tree(&source, destination, &record.content_digest)
}

fn fetch_remote(record: &ArtifactRecord, destination: &Path) -> Result<(), String> {
    if record.attempt_id.is_empty() {
        return Err(format!(
            "artifact {} is on {} and has no attempt id",
            record.name, record.host
        ));
    }
    let nixplane = std::env::var("NIXPLANE_BIN").unwrap_or_else(|_| "nixplane".into());
    let staging = destination.with_extension(format!("fetch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|err| err.to_string())?;
    let archive = staging.join(&record.output);
    let status = Command::new(&nixplane)
        .args([
            "execution",
            "fetch",
            &record.attempt_id,
            "--name",
            &record.output,
            "--out",
            &archive.display().to_string(),
        ])
        .status()
        .map_err(|err| format!("nixplane execution fetch: {err}"))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!(
            "nixplane execution fetch failed with {}",
            status.code().unwrap_or(1)
        ));
    }
    let unpacked = staging.join("unpacked");
    safe_extract(&archive, &unpacked)?;
    let tree = if (unpacked.join("manifest.json")).is_file() {
        unpacked
    } else {
        fs::read_dir(&unpacked)
            .map_err(|err| err.to_string())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.join("manifest.json").is_file())
            .ok_or_else(|| "fetched bundle has no manifest".to_owned())?
    };
    verify_tree(&tree, &record.content_digest)?;
    let result = publish_tree(&tree, destination, &record.content_digest);
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn verify_tree(directory: &Path, expected_digest: &str) -> Result<(), String> {
    let manifest_path = directory.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|_| format!("{} has no manifest", directory.display()))?;
    let body: Value =
        serde_json::from_str(&text).map_err(|err| format!("invalid manifest: {err}"))?;
    let digest = body
        .get("bundleSha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    if digest != expected_digest {
        return Err(format!(
            "manifest digest {digest} does not match {expected_digest}"
        ));
    }
    let files = body
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "bundle manifest has no files".to_owned())?;
    if files.is_empty() {
        return Err("bundle manifest has no files".into());
    }
    let mut checked = Vec::new();
    for item in files {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "bundle file entry is missing a name".to_owned())?;
        if name
            != Path::new(name)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
            || name.is_empty()
            || name == "."
            || name == ".."
        {
            return Err(format!(
                "bundle file name is not a single path segment: {name}"
            ));
        }
        let expect = item
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name} has no sha256"))?;
        let path = directory.join(name);
        if path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
            || !path.is_file()
        {
            return Err(format!("manifest names a missing file: {name}"));
        }
        let actual = sha256_file(&path)?;
        if actual != expect {
            return Err(format!("{name} sha256 {actual} does not match {expect}"));
        }
        checked.push((name.to_owned(), expect.to_owned()));
    }
    let recomputed = canonical_digest(&body, &checked)?;
    if recomputed != digest {
        return Err(format!(
            "recomputed digest {recomputed} does not match {digest}"
        ));
    }
    Ok(())
}

fn canonical_digest(body: &Value, files: &[(String, String)]) -> Result<String, String> {
    let mut lines = Vec::new();
    if is_stargate_manifest(body) {
        let toolchain = body.get("toolchain").and_then(Value::as_object);
        lines.push(text_field(body, "commit"));
        lines.push(text_field(body, "profile"));
        lines.push(toolchain_field(toolchain, "rustc"));
        lines.push(toolchain_field(toolchain, "wasmBindgen"));
        lines.push(toolchain_field(toolchain, "binaryen"));
    } else if body.get("digestAlgorithm").and_then(Value::as_str) == Some("nxr-artifact-v1") {
        lines.push("nxr-artifact-v1".to_owned());
    } else {
        return Err("artifact manifest has no recognized digest algorithm".into());
    }
    let mut files = files.to_vec();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, digest) in files {
        lines.push(name);
        lines.push(digest);
    }
    let payload = format!("{}\n", lines.join("\n"));
    Ok(hex_digest(payload.as_bytes()))
}

fn is_stargate_manifest(body: &Value) -> bool {
    [
        "commit",
        "profile",
        "wasm",
        "glue",
        "computeWasm",
        "computeGlue",
    ]
    .iter()
    .all(|field| {
        body.get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    }) && body.get("toolchain").and_then(Value::as_object).is_some()
}

fn text_field(body: &Value, field: &str) -> String {
    body.get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn toolchain_field(toolchain: Option<&serde_json::Map<String, Value>>, field: &str) -> String {
    toolchain
        .and_then(|fields| fields.get(field))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn publish_tree(source: &Path, destination: &Path, digest: &str) -> Result<(), String> {
    if destination.exists() && verify_tree(destination, digest).is_ok() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let stage = destination.with_extension(format!("stage-{}", std::process::id()));
    let backup = destination.with_extension(format!("backup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&stage);
    copy_dir(source, &stage)?;
    verify_tree(&stage, digest)?;
    let had_previous = destination.exists();
    if had_previous {
        let _ = fs::remove_dir_all(&backup);
        fs::rename(destination, &backup).map_err(|err| err.to_string())?;
    }
    if rename_should_fail() {
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        let _ = fs::remove_dir_all(&stage);
        return Err("stopped before replacing the destination".into());
    }
    if let Err(err) = fs::rename(&stage, destination) {
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        return Err(err.to_string());
    }
    if let Err(err) = verify_tree(destination, digest) {
        let _ = fs::remove_dir_all(destination);
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        return Err(err);
    }
    if had_previous {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

fn rename_should_fail() -> bool {
    #[cfg(test)]
    {
        FAIL_RENAME.with(|flag| flag.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn copy_dir(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|err| err.to_string())?;
    for entry in fs::read_dir(source).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let target = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_symlink() {
            return Err(format!("refusing symlink {}", entry.path().display()));
        }
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

pub fn safe_extract(archive: &Path, staging: &Path) -> Result<(), String> {
    let file = fs::File::open(archive).map_err(|err| err.to_string())?;
    let mut tar = tar::Archive::new(file);
    let entries = tar.entries().map_err(|err| format!("archive: {err}"))?;
    fs::create_dir_all(staging).map_err(|err| err.to_string())?;
    let root = staging.canonicalize().map_err(|err| err.to_string())?;
    let mut total = 0_u64;
    let mut entries_seen = 0_usize;
    for entry in entries {
        let mut entry = entry.map_err(|err| format!("archive: {err}"))?;
        let path = entry.path().map_err(|err| err.to_string())?;
        let name = path.to_string_lossy();
        if name.is_empty()
            || name.starts_with('/')
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("unsafe archive entry {name}"));
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(format!("unsafe archive entry {name}"));
        }
        if kind.is_file() {
            total = total.saturating_add(entry.header().size().unwrap_or(0));
            entries_seen += 1;
            if entries_seen > ENTRY_LIMIT || total > UNPACK_LIMIT {
                return Err("archive exceeds the unpack cap".into());
            }
        } else if kind.is_dir() {
            entries_seen += 1;
            if entries_seen > ENTRY_LIMIT {
                return Err("archive exceeds the unpack cap".into());
            }
        } else {
            return Err(format!("unsafe archive entry {name}"));
        }
        let target = staging.join(&path);
        if let Ok(canonical) = target.canonicalize()
            && canonical != root
            && !canonical.starts_with(&root)
        {
            return Err(format!("unsafe archive entry {name}"));
        }
        entry
            .unpack(&target)
            .map_err(|err| format!("archive: {err}"))?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    Ok(hex_digest(&bytes))
}

#[cfg(test)]
thread_local! {
    static FAIL_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;

    fn fixture(dir: &Path, body: &[u8]) -> String {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("mod.wasm"), body).unwrap();
        let file_hash = {
            let digest = Sha256::digest(body);
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let digest = canonical_digest(
            &json!({"digestAlgorithm": "nxr-artifact-v1"}),
            &[("mod.wasm".to_owned(), file_hash.clone())],
        )
        .unwrap();
        let manifest = json!({
            "bundleSha256": digest,
            "digestAlgorithm": "nxr-artifact-v1",
            "files": [{"name": "mod.wasm", "sha256": file_hash}],
            "version": 1
        });
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        digest
    }

    #[test]
    fn a_tampered_file_is_not_a_verified_object() {
        let root = tempfile::tempdir().unwrap();
        let digest = fixture(root.path(), b"wasm");
        verify_tree(root.path(), &digest).unwrap();
        fs::write(root.path().join("mod.wasm"), b"tampered").unwrap();
        assert!(verify_tree(root.path(), &digest).is_err());
    }

    #[test]
    fn materialize_keeps_the_previous_destination_when_the_object_is_corrupt() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let store = root.path().join(".nxr").join("artifacts");
        let object = store.join("objects").join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        fs::write(object.join("mod.wasm"), b"corrupt").unwrap();
        let record = json!({
            "attemptId": "",
            "bundleSha256": digest,
            "consumerInput": "bundle",
            "contentDigest": digest,
            "host": local_host(),
            "identity": {},
            "name": "bundle",
            "output": "bundle.tar",
            "producerRun": "run-1"
        });
        fs::create_dir_all(store.join("records")).unwrap();
        fs::write(
            store.join("records").join("bundle.json"),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();
        let destination = root
            .path()
            .join(".nxr")
            .join("artifacts")
            .join("materialized")
            .join("bundle");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("previous.txt"), "stay").unwrap();
        let error = materialize_inputs(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(root.path().to_path_buf()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
        )
        .unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert_eq!(
            fs::read_to_string(destination.join("previous.txt")).unwrap(),
            "stay"
        );
    }

    #[test]
    fn a_record_keeps_the_producer_and_consumer_fields() {
        let value = json!({
            "attemptId": "att_1",
            "contentDigest": "ab".repeat(32),
            "producerRun": "run-1",
            "host": "linux",
            "consumerInput": "bundle",
            "output": "bundle.tar",
            "name": "bundle",
            "identity": {"profile": "wasm-dev"}
        });
        let record = ArtifactRecord::from_value(&value).unwrap();
        assert_eq!(record.producer_run, "run-1");
        assert_eq!(record.consumer_input, "bundle");
        assert_eq!(record.attempt_id, "att_1");
        assert_eq!(record.output, "bundle.tar");
        assert_eq!(record.physical_host, "");
    }

    #[test]
    fn a_stale_manifest_digest_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let digest = fixture(root.path(), b"wasm");
        let file_hash = hex_digest(b"changed");
        fs::write(root.path().join("mod.wasm"), b"changed").unwrap();
        let manifest = json!({
            "bundleSha256": digest,
            "digestAlgorithm": "nxr-artifact-v1",
            "files": [{"name": "mod.wasm", "sha256": file_hash}],
            "version": 1
        });
        fs::write(
            root.path().join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let error = verify_tree(root.path(), digest.as_str()).unwrap_err();
        assert!(error.contains("recomputed"), "{error}");
    }

    #[test]
    fn a_forged_bundle_digest_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let digest = fixture(root.path(), b"wasm");
        let forged = "f".repeat(64);
        let text = fs::read_to_string(root.path().join("manifest.json")).unwrap();
        let mut body: Value = serde_json::from_str(&text).unwrap();
        body["bundleSha256"] = json!(forged);
        fs::write(
            root.path().join("manifest.json"),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();
        let error = verify_tree(root.path(), &forged).unwrap_err();
        assert!(error.contains("recomputed"), "{error}");
        let _ = digest;
    }

    #[test]
    fn two_linux_hosts_do_not_share_a_missing_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let present = root.path().join("present");
        let missing = root.path().join("missing");
        for (store, include_object) in [(&present, true), (&missing, false)] {
            let objects = store
                .join(".nxr")
                .join("artifacts")
                .join("objects")
                .join(&digest);
            if include_object {
                fs::create_dir_all(&objects).unwrap();
                fs::copy(source.join("mod.wasm"), objects.join("mod.wasm")).unwrap();
                fs::copy(source.join("manifest.json"), objects.join("manifest.json")).unwrap();
            }
            let record = json!({
                "attemptId": "",
                "bundleSha256": digest,
                "consumerInput": "bundle",
                "contentDigest": digest,
                "host": "linux",
                "physicalHost": if include_object { "nixdev" } else { "other-linux" },
                "facet": "system",
                "identity": {"profile": "wasm-dev"},
                "name": "bundle",
                "output": "bundle.tar",
                "producerRun": "run-1"
            });
            fs::create_dir_all(store.join(".nxr").join("artifacts").join("records")).unwrap();
            fs::write(
                store
                    .join(".nxr")
                    .join("artifacts")
                    .join("records")
                    .join("bundle.json"),
                serde_json::to_string(&record).unwrap(),
            )
            .unwrap();
        }
        materialize_inputs(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(present.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
        )
        .unwrap();
        assert!(
            present
                .join(".nxr/artifacts/materialized/bundle/mod.wasm")
                .is_file()
        );
        let error = materialize_inputs(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(missing),
                env: Default::default(),
            },
            &["bundle".to_owned()],
        )
        .unwrap_err();
        assert!(error.contains("not available locally"), "{error}");
    }

    #[test]
    fn a_damaged_destination_is_replaced_from_the_verified_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let store = root.path().join(".nxr").join("artifacts");
        let object = store.join("objects").join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        let destination = store.join("materialized").join("bundle");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("previous.txt"), "damaged").unwrap();
        write_record(root.path(), &digest);
        materialize_inputs(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(root.path().to_path_buf()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("mod.wasm")).unwrap(), b"wasm");
        assert!(!destination.join("previous.txt").exists());
    }

    #[test]
    fn a_failed_replace_restores_the_previous_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let store = root.path().join(".nxr").join("artifacts");
        let object = store.join("objects").join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        let destination = store.join("materialized").join("bundle");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("previous.txt"), "stay").unwrap();
        write_record(root.path(), &digest);
        FAIL_RENAME.with(|flag| flag.set(true));
        let error = materialize_inputs(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(root.path().to_path_buf()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
        )
        .unwrap_err();
        FAIL_RENAME.with(|flag| flag.set(false));
        assert!(error.contains("stopped before replacing"), "{error}");
        assert_eq!(
            fs::read_to_string(destination.join("previous.txt")).unwrap(),
            "stay"
        );
    }

    #[test]
    fn a_second_checkout_reuses_the_host_catalog() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let first = root.path().join("first");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let object = first
            .join(".nxr")
            .join("artifacts")
            .join("objects")
            .join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        write_record_identity(first.as_path(), &digest, "src-a");
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(first.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        let record_dirs = fs::read_dir(catalog.join("records")).unwrap();
        let keys: Vec<String> = record_dirs
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].len(), 64);
        assert!(
            keys[0]
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert!(
            catalog
                .join("records")
                .join(&keys[0])
                .join("bundle.json")
                .is_file()
        );
        let second = root.path().join("second");
        write_record_identity(second.as_path(), &digest, "src-a");
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(second.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        assert_eq!(
            fs::read(second.join(".nxr/artifacts/materialized/bundle/mod.wasm")).unwrap(),
            b"wasm"
        );
        let decision: Value = serde_json::from_str(
            &fs::read_to_string(second.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(decision["transfer"], "local");
        assert_eq!(decision["decision"], "hit");
    }

    #[test]
    fn explain_names_the_source_set_when_it_changes() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let checkout = root.path().join("checkout");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let object = checkout
            .join(".nxr")
            .join("artifacts")
            .join("objects")
            .join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        write_record_identity(checkout.as_path(), &digest, "src-new");
        let stored = catalog.join("records").join("src-old");
        fs::create_dir_all(&stored).unwrap();
        fs::write(
            stored.join("bundle.json"),
            serde_json::to_string(&json!({
                "attemptId": "",
                "contentDigest": digest,
                "host": "linux",
                "name": "bundle",
                "output": "bundle.tar",
                "identity": {"sourceSet": "src-old", "toolchain": "rust-1", "inputs": "aaa"}
            }))
            .unwrap(),
        )
        .unwrap();
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(checkout.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        let decision: Value = serde_json::from_str(
            &fs::read_to_string(checkout.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(decision["changedField"], "sourceSet");
        annotate_decision(
            checkout.as_path(),
            "bundle",
            Some(&json!(1200)),
            Some(&json!(40)),
            3,
            1,
        )
        .unwrap();
        let decision: Value = serde_json::from_str(
            &fs::read_to_string(checkout.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(decision["task_ms"], 1200);
        assert_eq!(decision["checkout_ms"], 40);
        assert_eq!(decision["chunks_resumed"], 3);
        assert!(decision.get("savings_ms").is_none());
        assert_eq!(decision["savingsBasis"], "unavailable");
    }

    #[test]
    fn stargate_inputs_key_the_catalog_not_the_source_set() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let source = root.path().join("object");
        let first_digest = fixture(&source, b"wasm");
        let other = root.path().join("other");
        let second_digest = fixture(&other, b"next");
        let first = stargate_record(&first_digest, "commit-a", "lattice-1");
        let same_inputs = stargate_record(&first_digest, "commit-a", "lattice-2");
        let other_commit = stargate_record(&second_digest, "commit-b", "lattice-1");
        let key = super::catalog_key(&first).unwrap();
        assert_eq!(key.len(), 64);
        assert!(!key.contains('/') && !key.contains('\\'));
        assert_eq!(super::catalog_key(&same_inputs).unwrap(), key);
        assert_ne!(super::catalog_key(&other_commit).unwrap(), key);

        let object = copy_fixture_object(root.path(), &source, &first_digest);
        super::publish_record_tree(&catalog, &first, &object).unwrap();
        let other_object = copy_fixture_object(root.path(), &other, &second_digest);
        let conflict = super::publish_record_tree(
            &catalog,
            &other_commit_same_key(&first, &second_digest),
            &other_object,
        )
        .unwrap_err();
        assert!(conflict.contains("catalog conflict"), "{conflict}");
        let stored =
            fs::read_to_string(catalog.join("records").join(&key).join("bundle.json")).unwrap();
        assert!(stored.contains(&first_digest));
        assert!(!stored.contains(&second_digest));

        let legacy = root.path().join("legacy");
        fs::create_dir_all(legacy.join("records").join("unscoped")).unwrap();
        fs::write(
            legacy.join("records").join("unscoped").join("bundle.json"),
            serde_json::to_string(&json!({
                "contentDigest": "ab".repeat(32),
                "name": "bundle",
                "output": "bundle.tar",
                "identity": first.identity
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(!super::catalog_object_ok(&legacy, &first));

        let matched = root.path().join("matched");
        let matched_object = matched.join("objects").join(&first_digest);
        fs::create_dir_all(&matched_object).unwrap();
        fs::copy(object.join("mod.wasm"), matched_object.join("mod.wasm")).unwrap();
        fs::copy(
            object.join("manifest.json"),
            matched_object.join("manifest.json"),
        )
        .unwrap();
        fs::create_dir_all(matched.join("records").join("unscoped")).unwrap();
        fs::write(
            matched.join("records").join("unscoped").join("bundle.json"),
            serde_json::to_string(&json!({
                "contentDigest": first_digest,
                "name": "bundle",
                "output": "bundle.tar",
                "identity": first.identity.clone()
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(super::catalog_object_ok(&matched, &first));

        let checkout = root.path().join("checkout");
        write_stargate_checkout(&checkout, &first_digest, "commit-a", "lattice-2");
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(checkout.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        assert_eq!(
            fs::read(checkout.join(".nxr/artifacts/materialized/bundle/mod.wasm")).unwrap(),
            b"wasm"
        );
        let decision: Value = serde_json::from_str(
            &fs::read_to_string(checkout.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(decision["decision"], "hit");
        assert_eq!(decision["transfer"], "local");
    }

    #[test]
    fn incomplete_stargate_identity_is_not_cacheable() {
        let digest = "ab".repeat(32);
        let mut missing_compiler = stargate_record(&digest, "commit-a", "lattice-1");
        missing_compiler.identity["toolchain"]["rustc"] = json!("");
        let error = super::catalog_key(&missing_compiler).unwrap_err();
        assert!(error.contains("no source identity"), "{error}");

        let mut clean = stargate_record(&digest, "commit-a", "lattice-1");
        clean.identity["dirty"] = json!("clean");
        let mut dirty = stargate_record(&digest, "commit-a", "lattice-1");
        dirty.identity["dirty"] = json!("dirty-1");
        assert_ne!(
            super::catalog_key(&clean).unwrap(),
            super::catalog_key(&dirty).unwrap()
        );

        let mut generic = super::ArtifactRecord::from_value(&json!({
            "contentDigest": digest,
            "name": "bundle",
            "output": "bundle.tar",
            "identity": {"sourceSet": "src-a"}
        }))
        .unwrap();
        assert!(
            super::catalog_key(&generic)
                .unwrap_err()
                .contains("no source identity")
        );
        generic.identity["toolchain"] = json!("rust-1");
        generic.identity["inputs"] = json!("aaa");
        assert!(super::catalog_key(&generic).is_ok());
    }

    #[test]
    fn two_writers_keep_one_catalog_record() {
        if let Ok(role) = std::env::var("NXR_CATALOG_CHILD") {
            let catalog = std::path::PathBuf::from(std::env::var("NXR_CATALOG_DIR").unwrap());
            let digest = std::env::var("NXR_CATALOG_DIGEST").unwrap();
            let object = std::path::PathBuf::from(std::env::var("NXR_CATALOG_OBJECT").unwrap());
            let record = stargate_record(&digest, "commit-a", "lattice-1");
            let result = super::publish_record_tree(&catalog, &record, &object);
            let report = catalog.join(format!("writer-{role}.txt"));
            let text = match result {
                Ok(()) => "ok".to_owned(),
                Err(err) => err,
            };
            fs::write(report, text).unwrap();
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let left_source = root.path().join("left");
        let right_source = root.path().join("right");
        let left_digest = fixture(&left_source, b"wasm-left");
        let right_digest = fixture(&right_source, b"wasm-right");
        let left_object = copy_fixture_object(root.path(), &left_source, &left_digest);
        let right_object = copy_fixture_object(root.path(), &right_source, &right_digest);
        fs::create_dir_all(&catalog).unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for (role, digest, object) in [
            ("left", &left_digest, &left_object),
            ("right", &right_digest, &right_object),
        ] {
            let child = std::process::Command::new(&exe)
                .arg("--exact")
                .arg("artifact::tests::two_writers_keep_one_catalog_record")
                .arg("--test-threads=1")
                .env("NXR_CATALOG_CHILD", role)
                .env("NXR_CATALOG_DIR", &catalog)
                .env("NXR_CATALOG_DIGEST", digest)
                .env("NXR_CATALOG_OBJECT", object)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            children.push(child);
        }
        for mut child in children {
            let status = child.wait().unwrap();
            assert!(status.success(), "{status}");
        }
        let left = fs::read_to_string(catalog.join("writer-left.txt")).unwrap();
        let right = fs::read_to_string(catalog.join("writer-right.txt")).unwrap();
        let outcomes = [left.trim(), right.trim()];
        assert_eq!(outcomes.iter().filter(|line| **line == "ok").count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|line| line.contains("catalog conflict"))
                .count(),
            1
        );
        let records = catalog.join("records");
        let mut record_files = Vec::new();
        for entry in fs::read_dir(&records).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                let file = path.join("bundle.json");
                if file.is_file() {
                    record_files.push(file);
                }
            }
        }
        assert_eq!(record_files.len(), 1);
        let stored = fs::read_to_string(&record_files[0]).unwrap();
        let winners = [&left_digest, &right_digest]
            .into_iter()
            .filter(|digest| stored.contains(*digest))
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn savings_are_measured_against_a_rebuild_baseline() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let checkout = root.path().join("checkout");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        write_record_identity(checkout.as_path(), &digest, "src-a");
        let decisions = checkout.join(".nxr").join("artifacts").join("decisions");
        fs::create_dir_all(&decisions).unwrap();
        fs::write(
            decisions.join("bundle.json"),
            serde_json::to_string(&json!({
                "decision": "rebuild",
                "digest": digest,
                "transfer": "local",
                "task": "bundle"
            }))
            .unwrap(),
        )
        .unwrap();
        super::annotate_decision_from(
            checkout.as_path(),
            "bundle",
            Some(&json!(5000)),
            None,
            0,
            0,
            Some(catalog.as_path()),
        )
        .unwrap();
        let rebuilt: Value =
            serde_json::from_str(&fs::read_to_string(decisions.join("bundle.json")).unwrap())
                .unwrap();
        assert_eq!(rebuilt["savingsBasis"], "unavailable");
        assert!(rebuilt.get("savings_ms").is_none());

        fs::write(
            decisions.join("bundle.json"),
            serde_json::to_string(&json!({
                "decision": "hit",
                "digest": digest,
                "transfer": "local",
                "task": "bundle"
            }))
            .unwrap(),
        )
        .unwrap();
        super::annotate_decision_from(
            checkout.as_path(),
            "bundle",
            Some(&json!(1200)),
            None,
            0,
            0,
            Some(catalog.as_path()),
        )
        .unwrap();
        let hit: Value =
            serde_json::from_str(&fs::read_to_string(decisions.join("bundle.json")).unwrap())
                .unwrap();
        assert_eq!(hit["savingsBasis"], "measured");
        assert_eq!(hit["savings_ms"], 3800);

        fs::write(
            checkout.join(".nxr/artifacts/records/bundle.json"),
            serde_json::to_string(&json!({
                "contentDigest": digest,
                "name": "bundle",
                "output": "bundle.tar",
                "identity": {"sourceSet": "src-b", "toolchain": "rust-1", "inputs": "bbb"}
            }))
            .unwrap(),
        )
        .unwrap();
        super::annotate_decision_from(
            checkout.as_path(),
            "bundle",
            Some(&json!(1000)),
            None,
            0,
            0,
            Some(catalog.as_path()),
        )
        .unwrap();
        let estimated: Value =
            serde_json::from_str(&fs::read_to_string(decisions.join("bundle.json")).unwrap())
                .unwrap();
        assert_eq!(estimated["savingsBasis"], "estimated");
        assert_eq!(estimated["savings_ms"], 4000);
    }

    #[test]
    fn the_same_catalog_explains_the_same_field() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let current = json!({
            "commit": "1",
            "profile": "new",
            "sourceSet": "same",
            "toolchain": "rust-1",
            "inputs": "aaa"
        });
        let record = super::ArtifactRecord::from_value(&json!({
            "contentDigest": "ab".repeat(32),
            "name": "bundle",
            "identity": current
        }))
        .unwrap();
        for (dir, identity) in [
            (
                "b-commit",
                json!({"commit": "0", "profile": "new", "sourceSet": "same", "toolchain": "rust-1", "inputs": "aaa"}),
            ),
            (
                "a-profile",
                json!({"commit": "1", "profile": "old", "sourceSet": "same", "toolchain": "rust-1", "inputs": "aaa"}),
            ),
        ] {
            let path = catalog.join("records").join(dir);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("bundle.json"),
                serde_json::to_string(&json!({
                    "contentDigest": "ab".repeat(32),
                    "name": "bundle",
                    "identity": identity
                }))
                .unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            super::changed_identity_field(&catalog, &record).as_deref(),
            Some("profile")
        );
        assert_eq!(
            super::changed_identity_field(&catalog, &record).as_deref(),
            Some("profile")
        );
    }

    #[test]
    fn cargo_reuse_is_not_recorded_without_a_receipt() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let checkout = root.path().join("checkout");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        write_record_identity(checkout.as_path(), &digest, "src-a");
        let object = checkout.join(".nxr/artifacts/objects").join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(checkout.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        let decision: Value = serde_json::from_str(
            &fs::read_to_string(checkout.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(decision["cargo"], "not-recorded");
        assert_eq!(decision["sccache"], "not-recorded");

        let receipts = checkout.join(".nxr/artifacts/receipts");
        fs::create_dir_all(&receipts).unwrap();
        fs::write(receipts.join("bundle.json"), r#"{"cargo":"reused"}"#).unwrap();
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(checkout.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        let observed: Value = serde_json::from_str(
            &fs::read_to_string(checkout.join(".nxr/artifacts/decisions/bundle.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(observed["cargo"], "reused");
    }

    #[test]
    fn publication_versions_identity_and_rejects_a_bad_producer() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let object = copy_fixture_object(root.path(), &source, &digest);
        let mut record = stargate_record(&digest, "commit-a", "lattice-1");
        super::publish_record_tree(&catalog, &record, &object).unwrap();
        let stored = fs::read_to_string(
            catalog
                .join("records")
                .join(super::catalog_key(&record).unwrap())
                .join("bundle.json"),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(value["identity"]["identitySchema"], 1);
        let legacy = super::ArtifactRecord::from_value(&json!({
            "contentDigest": digest,
            "name": "bundle",
            "identity": {"sourceSet": "src-a", "toolchain": "rust-1", "inputs": "aaa"}
        }))
        .unwrap();
        assert!(legacy.identity.get("identitySchema").is_none());

        record.identity["producer"] = json!("unknown-kind");
        let error = super::publish_record_tree(&catalog, &record, &object).unwrap_err();
        assert!(error.contains("unknown producer kind"), "{error}");

        let mut generic = super::ArtifactRecord::from_value(&json!({
            "contentDigest": digest,
            "name": "bundle",
            "identity": {
                "producer": "generic",
                "sourceSet": "src-a",
                "toolchain": "rust-1",
                "inputs": {"z": "1", "a": "2"}
            }
        }))
        .unwrap();
        let error = super::publish_record_tree(&catalog, &generic, &object).unwrap_err();
        assert!(error.contains("not canonical"), "{error}");
        generic.identity["inputs"] = json!("a=2\nz=1");
        super::publish_record_tree(&catalog, &generic, &object).unwrap();
    }

    #[test]
    fn a_changed_input_does_not_reuse_the_verified_match() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let first = root.path().join("first");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let object = first.join(".nxr/artifacts/objects").join(&digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        write_record_identity(first.as_path(), &digest, "src-a");
        super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(first),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap();
        let second = root.path().join("second");
        write_record_identity(second.as_path(), &digest, "src-b");
        let error = super::materialize_inputs_from(
            &ExecContext {
                path: String::new(),
                nix_override: None,
                cwd: Some(second.clone()),
                env: Default::default(),
            },
            &["bundle".to_owned()],
            Some(catalog.as_path()),
        )
        .unwrap_err();
        assert!(error.contains("not available locally"), "{error}");
        assert!(!second.join(".nxr/artifacts/materialized/bundle").exists());
    }

    #[test]
    fn retention_quarantines_a_bad_object_and_skips_a_locked_one() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        let source = root.path().join("object");
        let digest = fixture(&source, b"wasm");
        let record = stargate_record(&digest, "commit-a", "lattice-1");
        let object = copy_fixture_object(root.path(), &source, &digest);
        super::publish_record_tree(&catalog, &record, &object).unwrap();
        let key = super::catalog_key(&record).unwrap();
        fs::write(
            catalog.join("objects").join(&digest).join("mod.wasm"),
            b"corrupt",
        )
        .unwrap();
        super::retain_catalog(&catalog, 10).unwrap();
        assert!(
            !catalog
                .join("records")
                .join(&key)
                .join("bundle.json")
                .exists()
        );
        assert!(
            catalog
                .join("quarantine")
                .join(&key)
                .join("bundle.json")
                .is_file()
        );

        let fresh = root.path().join("fresh");
        let kept = fixture(&fresh, b"kept");
        let kept_record = stargate_record(&kept, "commit-b", "lattice-1");
        let kept_object = copy_fixture_object(root.path(), &fresh, &kept);
        let live = root.path().join("live");
        super::publish_record_tree(&live, &kept_record, &kept_object).unwrap();
        let lock_path = super::object_lock_path(&live, &kept);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        lock.lock_exclusive().unwrap();
        super::retain_catalog(&live, 0).unwrap();
        assert!(live.join("objects").join(&kept).join("mod.wasm").is_file());
        lock.unlock().unwrap();
        super::retain_catalog(&live, 0).unwrap();
        assert!(!live.join("objects").join(&kept).exists());
    }

    fn stargate_record(digest: &str, commit: &str, source_set: &str) -> super::ArtifactRecord {
        super::ArtifactRecord::from_value(&json!({
            "attemptId": "",
            "consumerInput": "bundle",
            "contentDigest": digest,
            "facet": "system",
            "host": "linux",
            "identity": {
                "commit": commit,
                "dirty": "dirty-1",
                "lockfile": "lock-1",
                "profile": "wasm-dev",
                "sourceSet": source_set,
                "toolchain": {
                    "binaryen": "1",
                    "rustc": "1.98.0",
                    "wasmBindgen": "0.2.127"
                }
            },
            "name": "bundle",
            "output": "bundle.tar",
            "physicalHost": "nixdev",
            "producerRun": "run-1"
        }))
        .unwrap()
    }

    fn other_commit_same_key(
        record: &super::ArtifactRecord,
        digest: &str,
    ) -> super::ArtifactRecord {
        let mut copy = record.clone();
        copy.content_digest = digest.to_owned();
        copy
    }

    fn copy_fixture_object(root: &Path, source: &Path, digest: &str) -> std::path::PathBuf {
        let object = root.join("staged").join(digest);
        fs::create_dir_all(&object).unwrap();
        fs::copy(source.join("mod.wasm"), object.join("mod.wasm")).unwrap();
        fs::copy(source.join("manifest.json"), object.join("manifest.json")).unwrap();
        object
    }

    fn write_stargate_checkout(root: &Path, digest: &str, commit: &str, source_set: &str) {
        let record = stargate_record(digest, commit, source_set);
        let records = root.join(".nxr").join("artifacts").join("records");
        fs::create_dir_all(&records).unwrap();
        fs::write(
            records.join("bundle.json"),
            serde_json::to_string(&json!({
                "attemptId": record.attempt_id,
                "consumerInput": record.consumer_input,
                "contentDigest": record.content_digest,
                "facet": record.facet,
                "host": record.host,
                "identity": record.identity,
                "name": record.name,
                "output": record.output,
                "physicalHost": record.physical_host,
                "producerRun": record.producer_run
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_record_identity(root: &Path, digest: &str, source_set: &str) {
        let record = json!({
            "attemptId": "",
            "bundleSha256": digest,
            "consumerInput": "bundle",
            "contentDigest": digest,
            "host": "linux",
            "physicalHost": "nixdev",
            "facet": "system",
            "identity": {"sourceSet": source_set, "toolchain": "rust-1", "inputs": "aaa"},
            "name": "bundle",
            "output": "bundle.tar",
            "producerRun": "run-1"
        });
        let records = root.join(".nxr").join("artifacts").join("records");
        fs::create_dir_all(&records).unwrap();
        fs::write(
            records.join("bundle.json"),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();
    }

    fn write_record(root: &Path, digest: &str) {
        let record = json!({
            "attemptId": "",
            "bundleSha256": digest,
            "consumerInput": "bundle",
            "contentDigest": digest,
            "host": "linux",
            "physicalHost": "nixdev",
            "facet": "system",
            "identity": {"profile": "wasm-dev"},
            "name": "bundle",
            "output": "bundle.tar",
            "producerRun": "run-1"
        });
        let records = root.join(".nxr").join("artifacts").join("records");
        fs::create_dir_all(&records).unwrap();
        fs::write(
            records.join("bundle.json"),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();
    }
}
