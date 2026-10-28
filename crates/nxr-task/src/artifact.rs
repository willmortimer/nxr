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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub name: String,
    pub identity: Value,
    pub content_digest: String,
    pub producer_run: String,
    pub host: String,
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
        if content_digest.len() != 64 {
            return Err("artifact record has no content digest".into());
        }
        Ok(Self {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
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
            output: value
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or("bundle.tar")
                .to_owned(),
        })
    }
}

pub fn local_host() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        other => other,
    }
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
        let record = load_record(&store, input)?;
        let destination = root
            .join(".nxr")
            .join("artifacts")
            .join("materialized")
            .join(input);
        if record.host.is_empty() || record.host == local_host() {
            restore_local(&store, &record, &destination)?;
        } else {
            fetch_remote(&record, &destination)?;
        }
    }
    Ok(())
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
    if destination.exists() {
        match verify_tree(destination, &record.content_digest) {
            Ok(()) => return Ok(()),
            Err(_) => {}
        }
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
    }
    Ok(())
}

fn publish_tree(source: &Path, destination: &Path, digest: &str) -> Result<(), String> {
    if destination.exists() {
        verify_tree(destination, digest)?;
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let stage = destination.with_extension(format!("stage-{}", std::process::id()));
    let _ = fs::remove_dir_all(&stage);
    copy_dir(source, &stage)?;
    verify_tree(&stage, digest)?;
    fs::rename(&stage, destination).map_err(|err| err.to_string())?;
    Ok(())
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
            if total > UNPACK_LIMIT {
                return Err("archive exceeds the unpack cap".into());
            }
        } else if !kind.is_dir() {
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
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
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
        let digest = {
            let digest = Sha256::digest(file_hash.as_bytes());
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let manifest = json!({
            "bundleSha256": digest,
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
        let _ = PathBuf::from(record.name);
    }
}
