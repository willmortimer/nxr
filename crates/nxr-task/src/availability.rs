//! Artifact availability. The scheduler may skip a node only when every
//! answer is yes. A missing answer is `not_measured`, which does not skip.

use serde::{Deserialize, Serialize};

/// One of the four reuse questions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The answer is yes.
    Yes,
    /// The answer is no.
    No,
    /// Nobody measured this.
    NotMeasured,
}

/// Whether a stored artifact can stand in for a new execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseDecision {
    /// Run the node.
    Execute,
    /// The stored artifact satisfies this consumer.
    Skip,
}

/// The four questions, kept separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactAvailability {
    /// Schema name.
    pub schema: String,
    /// A compatible artifact was produced by an eligible attempt.
    pub produced: Availability,
    /// The stored digest was verified.
    pub digest_verified: Availability,
    /// The intended consumer can read it.
    pub accessible: Availability,
    /// Platform, toolchain, source, and trust match.
    pub requirements_match: Availability,
}

impl ArtifactAvailability {
    /// Every question starts unmeasured.
    #[must_use]
    pub fn not_measured() -> Self {
        Self {
            schema: "artifact-availability-v1".to_owned(),
            produced: Availability::NotMeasured,
            digest_verified: Availability::NotMeasured,
            accessible: Availability::NotMeasured,
            requirements_match: Availability::NotMeasured,
        }
    }
}

/// What the current execution asks for. These values do not come from the receipt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentRequirements {
    /// Intended consumer.
    pub consumer: String,
    /// Toolchain the task requires now.
    pub toolchain: String,
    /// Platform the task requires now.
    pub platform: String,
    /// Source digest the task requires now.
    pub source_digest: String,
    /// Fingerprint of the producer operation. Empty means unmeasured.
    pub operation_digest: String,
}

/// The realized command for one node. This is not the producer output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProgram {
    /// Nix system the command was resolved for.
    pub system: String,
    /// Executable path.
    pub program: String,
    /// Arguments that belong to that executable.
    pub arguments: Vec<String>,
}

/// A reuse the scheduler accepted. The event stream records this separately from success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReuse {
    /// Task id.
    pub node: String,
    /// Verified content digest.
    pub digest: String,
    /// Producer attempt that wrote the receipt.
    pub producer_attempt: String,
    /// Where the bytes were read.
    pub storage_layer: String,
    /// Producer receipt that was checked against the current plan and the object bytes.
    pub verification_receipt: String,
}

/// Resolve reuse from the current requirements, a producer receipt, and the object bytes.
///
/// The receipt is `.nxr/artifacts/receipts/{node}.json`. It is not allowed to
/// supply the expected consumer, toolchain, platform, or source. A missing
/// input is `not_measured`.
#[must_use]
pub fn resolve_availability(
    root: &std::path::Path,
    node: &str,
    requirements: &CurrentRequirements,
) -> (ArtifactAvailability, Option<ArtifactReuse>) {
    let path = root
        .join(".nxr")
        .join("artifacts")
        .join("receipts")
        .join(format!("{node}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (ArtifactAvailability::not_measured(), None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (ArtifactAvailability::not_measured(), None);
    };
    let attempt = value
        .get("producerAttempt")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let outcome = value.get("outcome").and_then(|item| item.as_str());
    let produced = match (attempt.is_empty(), outcome) {
        (false, Some("succeeded")) => Availability::Yes,
        (_, Some(_)) => Availability::No,
        _ => Availability::NotMeasured,
    };
    let claimed = value
        .get("contentDigest")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let location = value.get("location").and_then(|item| item.as_str());
    let (digest_verified, accessible) = match location {
        Some(location) => match std::fs::read(location) {
            Ok(bytes) => {
                let actual = format!("sha256:{}", hex_sha256(&bytes));
                let verified = if claimed.is_empty() {
                    Availability::NotMeasured
                } else if actual.eq_ignore_ascii_case(claimed) {
                    Availability::Yes
                } else {
                    Availability::No
                };
                (verified, Availability::Yes)
            }
            Err(_) => (Availability::NotMeasured, Availability::No),
        },
        None => (Availability::NotMeasured, Availability::NotMeasured),
    };
    let checks = [
        compare_requirement(
            value.get("consumer").and_then(|item| item.as_str()),
            &requirements.consumer,
        ),
        compare_requirement(
            value.get("toolchain").and_then(|item| item.as_str()),
            &requirements.toolchain,
        ),
        compare_requirement(
            value.get("platform").and_then(|item| item.as_str()),
            &requirements.platform,
        ),
        compare_requirement(
            value.get("source").and_then(|item| item.as_str()),
            &requirements.source_digest,
        ),
        compare_requirement(
            value.get("operation").and_then(|item| item.as_str()),
            &requirements.operation_digest,
        ),
    ];
    let requirements_match = if checks.contains(&Some(false)) {
        Availability::No
    } else if checks.contains(&None) {
        Availability::NotMeasured
    } else {
        Availability::Yes
    };
    let availability = ArtifactAvailability {
        schema: "artifact-availability-v1".to_owned(),
        produced,
        digest_verified,
        accessible,
        requirements_match,
    };
    let reuse = if !claimed.is_empty() && !attempt.is_empty() {
        Some(ArtifactReuse {
            node: node.to_owned(),
            digest: claimed.to_owned(),
            producer_attempt: attempt.to_owned(),
            storage_layer: "local_file".to_owned(),
            verification_receipt: path.display().to_string(),
        })
    } else {
        None
    };
    (availability, reuse)
}

fn compare_requirement(recorded: Option<&str>, current: &str) -> Option<bool> {
    let recorded = recorded.filter(|text| !text.is_empty())?;
    if current.is_empty() {
        return None;
    }
    Some(recorded == current)
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Requirements for one plan node. Empty source means this node is not reused.
#[must_use]
pub fn current_requirements(
    root: &std::path::Path,
    node: &str,
    definition: &crate::schema::TaskDefinition,
    platform: Option<&str>,
    paths: &[String],
    program: Option<&ResolvedProgram>,
) -> CurrentRequirements {
    let toolchain = match definition
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(shell) => format!("{}+{shell}", definition.app),
        None => definition.app.clone(),
    };
    let platform = platform
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(host_platform);
    CurrentRequirements {
        consumer: node.to_owned(),
        toolchain,
        platform,
        source_digest: source_digest(root, paths),
        operation_digest: operation_digest(root, definition, program),
    }
}

/// One requirement record per plan node that still exists in the task document.
#[must_use]
pub fn requirements_for_plan(
    root: &std::path::Path,
    tasks: &std::collections::BTreeMap<String, crate::schema::TaskDefinition>,
    plan: &crate::plan_exec::ExecutionPlan,
    programs: &std::collections::BTreeMap<String, ResolvedProgram>,
) -> std::collections::BTreeMap<String, CurrentRequirements> {
    let mut requirements = std::collections::BTreeMap::new();
    for node in &plan.nodes {
        let Some(definition) = tasks.get(&node.id) else {
            continue;
        };
        requirements.insert(
            node.id.clone(),
            current_requirements(
                root,
                &node.id,
                definition,
                node.platform.as_deref(),
                &definition.paths,
                programs.get(&node.id),
            ),
        );
    }
    requirements
}

/// Relative output path for nodes that declare exactly one output.
#[must_use]
pub fn single_output_paths(
    tasks: &std::collections::BTreeMap<String, crate::schema::TaskDefinition>,
    plan: &crate::plan_exec::ExecutionPlan,
) -> std::collections::BTreeMap<String, String> {
    let mut paths = std::collections::BTreeMap::new();
    for node in &plan.nodes {
        let Some(definition) = tasks.get(&node.id) else {
            continue;
        };
        if definition.outputs.len() == 1 {
            paths.insert(node.id.clone(), definition.outputs[0].path.clone());
        }
    }
    paths
}

/// Record a successful producer. A missing output, or any count other than one, writes nothing.
///
/// # Errors
///
/// Returns an error when the output exists but the receipt or object cannot be stored.
pub fn record_producer_receipt(
    root: &std::path::Path,
    node: &str,
    requirements: &CurrentRequirements,
    output_relative: &str,
    producer_attempt: &str,
) -> std::io::Result<()> {
    if producer_attempt.is_empty() || !path_is_safe(output_relative) {
        return Ok(());
    }
    let source = root.join(output_relative);
    let bytes = match std::fs::read(&source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let digest = format!("sha256:{}", hex_sha256(&bytes));
    let objects = root.join(".nxr").join("artifacts").join("objects");
    std::fs::create_dir_all(&objects)?;
    let name = digest.replace(':', "_");
    let location = objects.join(&name);
    let temporary = objects.join(format!(".{name}.tmp"));
    std::fs::write(&temporary, &bytes)?;
    std::fs::rename(&temporary, &location)?;
    let receipt = serde_json::json!({
        "producerAttempt": producer_attempt,
        "outcome": "succeeded",
        "contentDigest": digest,
        "location": location.display().to_string(),
        "consumer": requirements.consumer,
        "toolchain": requirements.toolchain,
        "platform": requirements.platform,
        "source": requirements.source_digest,
        "operation": requirements.operation_digest,
    });
    let receipts = root.join(".nxr").join("artifacts").join("receipts");
    std::fs::create_dir_all(&receipts)?;
    let path = receipts.join(format!("{node}.json"));
    let temporary = receipts.join(format!(".{node}.tmp"));
    std::fs::write(&temporary, receipt.to_string())?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

/// Copy a verified object onto the declared output when that file is missing or different.
///
/// The bytes that are hashed are the bytes that are renamed into place. A symlink
/// in the output path is refused.
#[must_use]
pub fn restore_recorded_output(root: &std::path::Path, node: &str, relative: &str) -> bool {
    if !path_is_safe(relative) {
        return false;
    }
    let receipt_path = root
        .join(".nxr")
        .join("artifacts")
        .join("receipts")
        .join(format!("{node}.json"));
    let Ok(text) = std::fs::read_to_string(&receipt_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(location) = value.get("location").and_then(|item| item.as_str()) else {
        return false;
    };
    let Some(claimed) = value.get("contentDigest").and_then(|item| item.as_str()) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(location) else {
        return false;
    };
    let actual = format!("sha256:{}", hex_sha256(&bytes));
    if !actual.eq_ignore_ascii_case(claimed) {
        return false;
    }
    install_without_following_links(root, relative, &bytes)
}

fn install_without_following_links(root: &std::path::Path, relative: &str, bytes: &[u8]) -> bool {
    let Ok(mut current) = openat::Dir::open(root) else {
        return false;
    };
    let mut parts = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(name) = parts.pop() else {
        return false;
    };
    for part in parts {
        match current.metadata(part) {
            Ok(meta) if meta.simple_type() == openat::SimpleType::Symlink => return false,
            Ok(meta) if meta.simple_type() == openat::SimpleType::Dir => {
                let Ok(next) = current.sub_dir(part) else {
                    return false;
                };
                current = next;
            }
            Ok(_) => return false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if current.create_dir(part, 0o755).is_err() {
                    return false;
                }
                let Ok(next) = current.sub_dir(part) else {
                    return false;
                };
                current = next;
            }
            Err(_) => return false,
        }
    }
    match current.metadata(name) {
        Ok(meta) if meta.simple_type() == openat::SimpleType::Symlink => return false,
        Ok(meta) if meta.simple_type() == openat::SimpleType::File => {
            let Ok(mut existing) = current.open_file(name) else {
                return false;
            };
            let mut present = Vec::new();
            if std::io::Read::read_to_end(&mut existing, &mut present).is_err() {
                return false;
            }
            if present == bytes {
                return true;
            }
        }
        Ok(_) => return false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    let temporary = format!(".{name}.nxr-tmp");
    let Ok(mut file) = current.write_file(&temporary, 0o644) else {
        return false;
    };
    if std::io::Write::write_all(&mut file, bytes).is_err() {
        let _ = current.remove_file(&temporary);
        return false;
    }
    drop(file);
    current.local_rename(&temporary, name).is_ok()
}

fn operation_digest(
    root: &std::path::Path,
    definition: &crate::schema::TaskDefinition,
    program: Option<&ResolvedProgram>,
) -> String {
    let Some(program) = program else {
        return String::new();
    };
    if program.system.trim().is_empty() || program.program.trim().is_empty() {
        return String::new();
    }
    let flake_nix = root.join("flake.nix");
    let flake_lock = root.join("flake.lock");
    let (Ok(nix_bytes), Ok(lock_bytes)) = (std::fs::read(&flake_nix), std::fs::read(&flake_lock))
    else {
        return String::new();
    };
    if nix_bytes.is_empty() || lock_bytes.is_empty() {
        return String::new();
    }
    let Ok(task_json) = serde_json::to_string(definition) else {
        return String::new();
    };
    let mut payload = Vec::new();
    payload.extend_from_slice(b"task\0");
    payload.extend_from_slice(task_json.as_bytes());
    payload.extend_from_slice(b"\0nix\0");
    payload.extend_from_slice(&nix_bytes);
    payload.extend_from_slice(b"\0lock\0");
    payload.extend_from_slice(&lock_bytes);
    payload.extend_from_slice(b"\0system\0");
    payload.extend_from_slice(program.system.as_bytes());
    payload.extend_from_slice(b"\0program\0");
    payload.extend_from_slice(program.program.as_bytes());
    for argument in &program.arguments {
        payload.extend_from_slice(b"\0arg\0");
        payload.extend_from_slice(argument.as_bytes());
    }
    format!("sha256:{}", hex_sha256(&payload))
}

fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn source_digest(root: &std::path::Path, paths: &[String]) -> String {
    if paths.is_empty() || tree_is_dirty(root) {
        return String::new();
    }
    let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut payload = Vec::new();
    for path in sorted {
        if !path_is_safe(path) {
            return String::new();
        }
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            return String::new();
        };
        payload.extend_from_slice(path.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&bytes);
        payload.push(0);
    }
    format!("sha256:{}", hex_sha256(&payload))
}

fn tree_is_dirty(root: &std::path::Path) -> bool {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
    else {
        return true;
    };
    !output.status.success() || !output.stdout.is_empty()
}

fn path_is_safe(path: &str) -> bool {
    let path = std::path::Path::new(path);
    !path.as_os_str().is_empty()
        && path.is_relative()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// Skip only when all four answers are yes.
#[must_use]
pub fn reuse_decision(record: &ArtifactAvailability) -> ReuseDecision {
    if record.produced == Availability::Yes
        && record.digest_verified == Availability::Yes
        && record.accessible == Availability::Yes
        && record.requirements_match == Availability::Yes
    {
        ReuseDecision::Skip
    } else {
        ReuseDecision::Execute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warm() -> ArtifactAvailability {
        ArtifactAvailability {
            schema: "artifact-availability-v1".into(),
            produced: Availability::Yes,
            digest_verified: Availability::Yes,
            accessible: Availability::Yes,
            requirements_match: Availability::Yes,
        }
    }

    #[test]
    fn stargate_cases_stay_separate() {
        assert_eq!(reuse_decision(&warm()), ReuseDecision::Skip);
        let mut cold = warm();
        cold.produced = Availability::No;
        assert_eq!(reuse_decision(&cold), ReuseDecision::Execute);
        let mut changed = warm();
        changed.requirements_match = Availability::No;
        assert_eq!(reuse_decision(&changed), ReuseDecision::Execute);
        let mut corrupt = warm();
        corrupt.digest_verified = Availability::No;
        assert_eq!(reuse_decision(&corrupt), ReuseDecision::Execute);
        let mut mismatch = warm();
        mismatch.accessible = Availability::No;
        assert_eq!(reuse_decision(&mismatch), ReuseDecision::Execute);
        assert_eq!(
            reuse_decision(&ArtifactAvailability::not_measured()),
            ReuseDecision::Execute
        );
    }

    #[test]
    fn a_record_is_not_measured_until_every_answer_is_present() {
        let root = tempfile::tempdir().unwrap();
        let receipts = root.path().join(".nxr/artifacts/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        let location = root.path().join("mod.wasm");
        let bytes = b"wasm-bytes";
        std::fs::write(&location, bytes).unwrap();
        let digest = format!("sha256:{}", hex_sha256(bytes));
        let body = serde_json::json!({
            "contentDigest": digest,
            "producerAttempt": "att",
            "outcome": "succeeded",
            "location": location,
            "consumer": "worker-a",
            "toolchain": "wasm-bindgen",
            "platform": "wasm32",
            "source": "tree"
        });
        std::fs::write(receipts.join("stargate.json"), body.to_string()).unwrap();
        let changed = CurrentRequirements {
            consumer: "worker-a".into(),
            toolchain: "wasm-bindgen".into(),
            platform: "wasm32".into(),
            source_digest: "changed".into(),
            operation_digest: "sha256:op".into(),
        };
        let (availability, _) = resolve_availability(root.path(), "stargate", &changed);
        assert_eq!(availability.produced, Availability::Yes);
        assert_eq!(availability.digest_verified, Availability::Yes);
        assert_eq!(availability.accessible, Availability::Yes);
        assert_eq!(availability.requirements_match, Availability::No);
        assert_eq!(reuse_decision(&availability), ReuseDecision::Execute);
        std::fs::write(&location, b"corrupt").unwrap();
        let (corrupt, _) = resolve_availability(root.path(), "stargate", &changed);
        assert_eq!(corrupt.digest_verified, Availability::No);
        assert_eq!(
            resolve_availability(root.path(), "missing", &changed)
                .0
                .produced,
            Availability::NotMeasured
        );
    }

    #[test]
    fn restore_refuses_a_symlinked_parent_and_installs_the_hashed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let bytes = b"wasm-bytes";
        let digest = format!("sha256:{}", hex_sha256(bytes));
        let objects = root.path().join(".nxr/artifacts/objects");
        std::fs::create_dir_all(&objects).unwrap();
        let location = objects.join("object");
        std::fs::write(&location, bytes).unwrap();
        let receipts = root.path().join(".nxr/artifacts/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join("worker.json"),
            serde_json::json!({
                "contentDigest": digest,
                "location": location.display().to_string(),
            })
            .to_string(),
        )
        .unwrap();
        assert!(!restore_recorded_output(
            root.path(),
            "worker",
            "linked/worker.wasm"
        ));
        assert!(!outside.path().join("worker.wasm").exists());
        assert!(restore_recorded_output(
            root.path(),
            "worker",
            "out/worker.wasm"
        ));
        assert_eq!(
            std::fs::read(root.path().join("out/worker.wasm")).unwrap(),
            bytes
        );
    }
}
