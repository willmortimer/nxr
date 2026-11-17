//! Freeze admitted dirty manifests under a run directory.
//!
//! The caller supplies the JSON. These functions do not read a working tree.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::identity::{DirtySnapshotEntry, dirty_snapshot_digest, hex_digest};

const DIRTY_BUNDLE_SCHEMA: &str = "dirty-bundle-v1";

/// Freeze admitted dirty bytes under `{run_dir}/source/{source_set_id}/`.
///
/// A bare files document is stored at `{member}.dirty.json`. A
/// `dirty-bundle-v1` document stores each `manifests` entry as its own member
/// file and also writes `bundle.json`. An existing file is left in place when
/// its dirty digest matches. A different digest is an error and does not
/// change that file.
///
/// # Errors
///
/// Returns an error when `member` is not a single path segment, the JSON is
/// not a files document or a dirty bundle, or a frozen file has a different
/// digest.
pub fn freeze_dirty_manifest(
    run_dir: &Path,
    source_set_id: &str,
    member: &str,
    manifest_json: &str,
) -> Result<PathBuf, String> {
    require_source_set_id(source_set_id)?;
    require_member_segment(member)?;
    let value: Value =
        serde_json::from_str(manifest_json).map_err(|err| format!("dirty manifest: {err}"))?;
    if !value.is_object() {
        return Err("dirty manifest has no files".to_owned());
    }
    let dir = run_dir.join("source").join(source_set_id);
    if value.get("schema").and_then(Value::as_str) == Some(DIRTY_BUNDLE_SCHEMA) {
        return freeze_bundle(&dir, manifest_json, &value);
    }
    if value.get("files").and_then(Value::as_array).is_none() {
        return Err("dirty manifest has no files".to_owned());
    }
    digest_manifest_json(manifest_json)?;
    let path = dir.join(format!("{member}.dirty.json"));
    if needs_write(&path, manifest_json, digest_manifest_json)? {
        write_atomic(&path, manifest_json)?;
    }
    Ok(path)
}

/// Read one frozen member manifest and require `expected_dirty_id`.
///
/// Only `{run_dir}/source/{source_set_id}/{member}.dirty.json` is read.
///
/// # Errors
///
/// Returns `frozen dirty manifest is unavailable` when that file is missing.
/// Returns `frozen dirty manifest does not match the admitted snapshot` when
/// its digest differs from `expected_dirty_id`.
pub fn frozen_manifest(
    run_dir: &Path,
    source_set_id: &str,
    member: &str,
    expected_dirty_id: &str,
) -> Result<String, String> {
    require_source_set_id(source_set_id)?;
    require_member_segment(member)?;
    let path = run_dir
        .join("source")
        .join(source_set_id)
        .join(format!("{member}.dirty.json"));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err("frozen dirty manifest is unavailable".to_owned());
        }
        Err(err) => return Err(format!("frozen dirty manifest is unavailable: {err}")),
    };
    let digest = digest_manifest_json(&text)?;
    if digest != expected_dirty_id {
        return Err("frozen dirty manifest does not match the admitted snapshot".to_owned());
    }
    Ok(text)
}

/// Hash a files document the same way as NixPlane `digest_dirty_manifest`.
///
/// For kind `file`, the payload is the sha256 hex of the decoded `bytes_hex`.
/// Every other kind uses the `payload` string. The id is then
/// [`dirty_snapshot_digest`].
fn digest_manifest_json(json: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("dirty manifest: {err}"))?;
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "dirty manifest has no files".to_owned())?;
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mode = file
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("100644")
            .to_owned();
        let kind = file
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_owned();
        let payload = if kind == "file" {
            let hex_text = file.get("bytes_hex").and_then(Value::as_str).unwrap_or("");
            let bytes = decode_hex(hex_text)?;
            hex_digest(Sha256::digest(&bytes).as_slice())
        } else {
            file.get("payload")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        entries.push(DirtySnapshotEntry {
            path,
            mode,
            kind,
            payload,
        });
    }
    Ok(dirty_snapshot_digest(&entries))
}

fn freeze_bundle(dir: &Path, manifest_json: &str, value: &Value) -> Result<PathBuf, String> {
    let manifests = value
        .get("manifests")
        .and_then(Value::as_object)
        .ok_or_else(|| "dirty bundle has no manifests".to_owned())?;
    let mut planned = Vec::with_capacity(manifests.len());
    for (name, entry) in manifests {
        require_member_segment(name)?;
        let entry_json =
            serde_json::to_string(entry).map_err(|err| format!("dirty manifest: {err}"))?;
        digest_manifest_json(&entry_json)?;
        planned.push((dir.join(format!("{name}.dirty.json")), entry_json));
    }
    let bundle_path = dir.join("bundle.json");
    let mut write_member = Vec::with_capacity(planned.len());
    for (path, contents) in &planned {
        write_member.push(needs_write(path, contents, digest_manifest_json)?);
    }
    let write_bundle = needs_write_exact(&bundle_path, manifest_json)?;
    for ((path, contents), write) in planned.iter().zip(write_member) {
        if write {
            write_atomic(path, contents)?;
        }
    }
    if write_bundle {
        write_atomic(&bundle_path, manifest_json)?;
    }
    Ok(bundle_path)
}

fn require_source_set_id(source_set_id: &str) -> Result<(), String> {
    if source_set_id.is_empty()
        || source_set_id == "."
        || source_set_id == ".."
        || source_set_id.contains('/')
        || source_set_id.contains('\\')
    {
        return Err("source set id is not a single path segment".to_owned());
    }
    Ok(())
}

fn require_member_segment(member: &str) -> Result<(), String> {
    if member.is_empty() || member.contains('/') || member.contains('\\') || member.contains('.') {
        return Err("dirty member is not a single path segment".to_owned());
    }
    Ok(())
}

fn needs_write(
    path: &Path,
    contents: &str,
    digest: fn(&str) -> Result<String, String>,
) -> Result<bool, String> {
    match fs::read_to_string(path) {
        Ok(existing) => {
            let existing_digest = digest(&existing)?;
            let new_digest = digest(contents)?;
            if existing_digest != new_digest {
                return Err(format!(
                    "dirty manifest {} already frozen with a different snapshot",
                    path.display()
                ));
            }
            Ok(false)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) => Err(format!("read frozen dirty manifest: {err}")),
    }
}

fn needs_write_exact(path: &Path, contents: &str) -> Result<bool, String> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == contents => Ok(false),
        Ok(_) => Err(format!(
            "dirty bundle {} already frozen with a different snapshot",
            path.display()
        )),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) => Err(format!("read frozen dirty manifest: {err}")),
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create source directory: {err}"))?;
    }
    let temporary = temp_sibling(path);
    if let Err(err) = fs::write(&temporary, contents) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("write dirty manifest: {err}"));
    }
    if let Err(err) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("rename dirty manifest: {err}"));
    }
    Ok(())
}

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dirty.json");
    path.with_file_name(format!(".{name}.tmp"))
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("file bytes are not hex".to_owned());
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let hi = hex_nibble(raw[index])?;
        let lo = hex_nibble(raw[index + 1])?;
        bytes.push((hi << 4) | lo);
        index += 2;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("file bytes are not hex".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn freeze_keeps_admitted_hi_bytes_after_caller_source_changes() {
        let run = tempfile::tempdir().expect("tempdir");
        let original =
            r#"{"files":[{"path":"a.txt","mode":"100644","kind":"file","bytes_hex":"6869"}]}"#;
        let dirty_id = digest_manifest_json(original).expect("digest");
        let frozen_path =
            freeze_dirty_manifest(run.path(), "src_c12", "primary", original).expect("freeze");
        assert_eq!(
            frozen_path,
            run.path()
                .join("source")
                .join("src_c12")
                .join("primary.dirty.json")
        );

        let mut caller_source = original.to_owned();
        caller_source = caller_source.replace("6869", "6e6f");
        assert_ne!(caller_source, original);

        let frozen = frozen_manifest(run.path(), "src_c12", "primary", &dirty_id).expect("frozen");
        assert!(frozen.contains("6869"), "{frozen}");
        assert_eq!(frozen, original);

        let err = freeze_dirty_manifest(run.path(), "src_c12", "primary", &caller_source)
            .expect_err("different bytes must not replace the freeze");
        assert!(!err.is_empty(), "{err}");
        let on_disk = fs::read_to_string(&frozen_path).expect("frozen file");
        assert!(on_disk.contains("6869"), "{on_disk}");
        assert!(!on_disk.contains("6e6f"), "{on_disk}");
    }
}
