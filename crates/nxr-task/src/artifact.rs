//! Verified artifact materialization for workflow inputs.
//!
//! A record names the producer, the content digest, and the consumer input.
//! Materialization hashes every file the manifest lists before it replaces a
//! destination. A failed check leaves the previous destination in place.

use std::fs;
use std::path::Path;
use std::process::Command;

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
        let destination = root
            .join(".nxr")
            .join("artifacts")
            .join("materialized")
            .join(input);
        let object = store.join("objects").join(&record.content_digest);
        if verify_tree(&object, &record.content_digest).is_ok() {
            restore_local(&store, &record, &destination)?;
            write_materialize_decision(&store, input, &record, "local")?;
        } else if object.join("manifest.json").is_file() {
            verify_tree(&object, &record.content_digest)?;
        } else if !record.attempt_id.is_empty() {
            fetch_remote(&record, &destination)?;
            write_materialize_decision(&store, input, &record, "fetch")?;
        } else {
            return Err(format!(
                "artifact {input} is not available locally and has no attempt id"
            ));
        }
    }
    Ok(())
}

fn write_materialize_decision(
    store: &Path,
    input: &str,
    record: &ArtifactRecord,
    transfer: &str,
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
        "artifact": "hit",
        "cargo": "reused",
        "decision": "hit",
        "digest": record.content_digest,
        "facet": record.facet,
        "identityFields": identity_fields,
        "physicalHost": record.physical_host,
        "reason": "",
        "sccache": "not-recorded",
        "task": input,
        "transfer": transfer,
    });
    fs::write(path, format!("{body}\n")).map_err(|err| err.to_string())
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
