//! Generic diagnostic artifact declarations and independently verified bytes.
//!
//! A child's `verified` property is never evidence. Only `verify_local_cas`
//! constructs the non-deserializable proof accepted by the journal publisher.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ARTIFACT_REF_V1: &str = "nxr-artifact-ref-v1";
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ArtifactLocation {
    LocalCas { object: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub schema_version: String,
    pub name: String,
    pub producer: String,
    pub kind: String,
    pub media_type: String,
    pub digest: String,
    pub location: ArtifactLocation,
    #[serde(default)]
    pub verified: bool,
}

/// An in-process proof, deliberately not deserializable or publicly constructible.
#[derive(Clone, Debug)]
pub struct VerifiedArtifactRef(ArtifactRef);

impl VerifiedArtifactRef {
    #[must_use]
    pub fn reference(&self) -> &ArtifactRef {
        &self.0
    }
}

impl ArtifactRef {
    /// Accept a generic marker or the existing Lattice/Stargate marker.
    /// Regardless of what the child claims, the result is unverified.
    pub fn parse_declaration(line: &str) -> Result<Self, String> {
        let line = line.trim();
        let mut reference =
            if let Some(payload) = line.strip_prefix("lattice: diagnostic-artifact ") {
                let value: serde_json::Value =
                    serde_json::from_str(payload).map_err(|error| error.to_string())?;
                if value.get("schema").and_then(serde_json::Value::as_str)
                    != Some("lattice.diagnostic.artifact.v1")
                    || value.get("kind").and_then(serde_json::Value::as_str)
                        != Some("stargate-recording")
                {
                    return Err("unsupported Lattice artifact marker".into());
                }
                let digest = value
                    .get("sha256")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("artifact has no sha256")?;
                let object = value
                    .get("object")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("artifact has no object")?;
                Self {
                    schema_version: ARTIFACT_REF_V1.into(),
                    name: "failure-recording".into(),
                    producer: "stargate".into(),
                    kind: "diagnostic-recording".into(),
                    media_type: "application/json".into(),
                    digest: format!("sha256:{digest}"),
                    location: ArtifactLocation::LocalCas {
                        object: object.into(),
                    },
                    verified: false,
                }
            } else {
                let payload = line.strip_prefix("nxr: artifact ").unwrap_or(line);
                serde_json::from_str::<Self>(payload).map_err(|error| error.to_string())?
            };
        reference.verified = false;
        reference.validate()?;
        Ok(reference)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ARTIFACT_REF_V1 {
            return Err("unsupported artifact reference schema".into());
        }
        for value in [&self.name, &self.producer, &self.kind, &self.media_type] {
            if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return Err("invalid artifact metadata".into());
            }
        }
        let digest = self
            .digest
            .strip_prefix("sha256:")
            .ok_or("artifact digest must use sha256")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid artifact sha256".into());
        }
        let ArtifactLocation::LocalCas { object } = &self.location;
        let components: Vec<_> = object.split('/').collect();
        if object.len() > 1024
            || object.contains('\\')
            || object.chars().any(char::is_control)
            || components.len() < 3
            || components[0] != "objects"
            || components[1] != digest
            || components
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err("artifact object must be a confined objects/<digest>/file path".into());
        }
        Ok(())
    }

    /// Open each path component relative to a pinned directory descriptor with
    /// no symlink following, then hash a regular file with bounded memory.
    pub fn verify_local_cas(&self, store: &Path) -> Result<VerifiedArtifactRef, String> {
        self.validate()?;
        let ArtifactLocation::LocalCas { object } = &self.location;
        let mut directory =
            openat::Dir::open(store).map_err(|error| format!("open artifact store: {error}"))?;
        let mut components = object.split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_some() {
                directory = directory
                    .sub_dir(component)
                    .map_err(|error| format!("open artifact directory: {error}"))?;
                continue;
            }
            // Refuse existing devices/FIFOs before opening: opening a named
            // pipe for reading could otherwise block before fstat can reject it.
            let entry = directory
                .metadata(component)
                .map_err(|error| format!("inspect artifact: {error}"))?;
            if !entry.is_file() || entry.len() > MAX_ARTIFACT_BYTES {
                return Err("artifact must be a regular file within the 64 MiB limit".into());
            }
            let mut file = directory
                .open_file(component)
                .map_err(|error| format!("open artifact: {error}"))?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
                return Err("artifact must be a regular file within the 64 MiB limit".into());
            }
            let mut hash = Sha256::new();
            let mut total = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let length = file
                    .read(&mut buffer)
                    .map_err(|error| format!("read artifact: {error}"))?;
                if length == 0 {
                    break;
                }
                total = total.saturating_add(length as u64);
                if total > MAX_ARTIFACT_BYTES {
                    return Err("artifact grew beyond the 64 MiB limit".into());
                }
                hash.update(&buffer[..length]);
            }
            let actual = format!("sha256:{:x}", hash.finalize());
            if actual != self.digest.to_ascii_lowercase() {
                return Err(format!(
                    "artifact digest mismatch: expected {}, observed {actual}",
                    self.digest
                ));
            }
            let mut reference = self.clone();
            reference.verified = true;
            return Ok(VerifiedArtifactRef(reference));
        }
        Err("artifact has no file".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(bytes: &[u8]) -> ArtifactRef {
        let digest = format!("{:x}", Sha256::digest(bytes));
        ArtifactRef {
            schema_version: ARTIFACT_REF_V1.into(),
            name: "recording".into(),
            producer: "test".into(),
            kind: "diagnostic".into(),
            media_type: "application/octet-stream".into(),
            digest: format!("sha256:{digest}"),
            location: ArtifactLocation::LocalCas {
                object: format!("objects/{digest}/recording.json"),
            },
            verified: false,
        }
    }

    #[test]
    fn child_cannot_claim_verification() {
        let mut reference = reference(b"recording");
        reference.verified = true;
        let parsed =
            ArtifactRef::parse_declaration(&serde_json::to_string(&reference).expect("json"))
                .expect("declaration");
        assert!(!parsed.verified);
    }

    #[test]
    fn digest_verification_is_independent_of_the_declaration() {
        let store = tempfile::tempdir().expect("store");
        let reference = reference(b"recording");
        let ArtifactLocation::LocalCas { object } = &reference.location;
        let path = store.path().join(object);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"recording").expect("write");
        assert!(
            reference
                .verify_local_cas(store.path())
                .expect("verify")
                .reference()
                .verified
        );
        std::fs::write(&path, b"tampered").expect("tamper");
        assert!(reference.verify_local_cas(store.path()).is_err());
    }

    #[test]
    fn unsafe_object_paths_are_rejected() {
        for object in [
            "/etc/passwd",
            "objects/../secret",
            "objects/a/../../secret",
            "objects/a\\b/file",
            "objects//file",
        ] {
            let mut reference = reference(b"bytes");
            reference.location = ArtifactLocation::LocalCas {
                object: object.into(),
            };
            assert!(reference.validate().is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_inside_the_store_are_rejected() {
        let store = tempfile::tempdir().expect("store");
        let outside = tempfile::tempdir().expect("outside");
        let reference = reference(b"recording");
        let ArtifactLocation::LocalCas { object } = &reference.location;
        let path = store.path().join(object);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(outside.path().join("recording"), b"recording").expect("write");
        std::os::unix::fs::symlink(outside.path().join("recording"), path).expect("symlink");
        assert!(reference.verify_local_cas(store.path()).is_err());
    }
}
