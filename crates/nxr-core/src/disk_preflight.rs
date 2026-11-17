//! Disk free-space preflight before writing outputs.

use std::path::Path;

/// Outcome of a disk free-space preflight check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskDecision {
    pub allowed: bool,
    pub available_bytes: Option<u64>,
    pub reserve_bytes: u64,
    pub reason: String,
}

/// Bytes to keep free before a managed write.
///
/// `NXR_DISK_RESERVE_BYTES` wins when it parses. Otherwise the floor is
/// 512 MiB and the cap is 2 GiB, taken from one quarter of `host_memory_mib`.
#[must_use]
pub fn disk_reserve_bytes(reserve_env: Option<&str>, host_memory_mib: u64) -> u64 {
    if let Some(parsed) = reserve_env.and_then(|value| value.parse::<u64>().ok()) {
        return parsed;
    }
    const FLOOR: u64 = 512 * 1024 * 1024;
    const CAP: u64 = 2 * 1024 * 1024 * 1024;
    let from_host = host_memory_mib
        .saturating_div(4)
        .saturating_mul(1024 * 1024);
    from_host.clamp(FLOOR, CAP)
}

/// Check whether `output_dir` has at least `reserve_bytes` free.
///
/// Creates no files and deletes nothing. On Unix, queries `statvfs` for
/// `output_dir` when it exists, otherwise the nearest existing ancestor.
#[must_use]
pub fn disk_preflight(output_dir: &Path, reserve_bytes: u64) -> DiskDecision {
    #[cfg(unix)]
    {
        disk_preflight_unix(output_dir, reserve_bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = output_dir;
        DiskDecision {
            allowed: false,
            available_bytes: None,
            reserve_bytes,
            reason: "disk availability is unknown".into(),
        }
    }
}

#[cfg(unix)]
fn disk_preflight_unix(output_dir: &Path, reserve_bytes: u64) -> DiskDecision {
    let Some(probe) = nearest_existing(output_dir) else {
        return DiskDecision {
            allowed: false,
            available_bytes: None,
            reserve_bytes,
            reason: "disk availability is unknown".into(),
        };
    };

    match nix::sys::statvfs::statvfs(probe) {
        Ok(stat) => {
            let available = available_bytes(&stat);
            if available < reserve_bytes {
                DiskDecision {
                    allowed: false,
                    available_bytes: Some(available),
                    reserve_bytes,
                    reason: format!(
                        "insufficient disk: available {available} < reserve {reserve_bytes}"
                    ),
                }
            } else {
                DiskDecision {
                    allowed: true,
                    available_bytes: Some(available),
                    reserve_bytes,
                    reason: format!(
                        "enough disk: available {available} >= reserve {reserve_bytes}"
                    ),
                }
            }
        }
        Err(_) => DiskDecision {
            allowed: false,
            available_bytes: None,
            reserve_bytes,
            reason: "disk availability is unknown".into(),
        },
    }
}

/// Free bytes from a `statvfs` result.
///
/// `fragment_size` is `u64` on both hosts. `blocks_available` is `u32` on
/// macOS and `u64` on Linux, so one cast fails one of the two lints.
#[cfg(unix)]
fn available_bytes(stat: &nix::sys::statvfs::Statvfs) -> u64 {
    #[cfg(target_os = "linux")]
    let blocks = stat.blocks_available();
    #[cfg(not(target_os = "linux"))]
    let blocks = u64::from(stat.blocks_available());
    stat.fragment_size().saturating_mul(blocks)
}

/// Walk from `path` toward the root, returning the nearest existing ancestor.
///
/// Does not create any path components.
#[cfg(unix)]
fn nearest_existing(path: &Path) -> Option<&Path> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn disk_reserve_uses_the_env_or_a_host_floor() {
        assert_eq!(disk_reserve_bytes(Some("4096"), u64::MAX), 4096);
        assert_eq!(disk_reserve_bytes(None, 64), 512 * 1024 * 1024);
        assert_eq!(disk_reserve_bytes(None, u64::MAX), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn disk_preflight_temp_dir_with_reserve_one_allows() {
        let decision = disk_preflight(&std::env::temp_dir(), 1);
        assert!(decision.allowed, "reason={}", decision.reason);
        assert!(decision.available_bytes.is_some());
        assert_eq!(decision.reserve_bytes, 1);
        assert!(decision.reason.contains("enough disk"));
    }

    #[test]
    fn disk_preflight_reserve_u64_max_refuses_with_insufficient_disk() {
        let decision = disk_preflight(&std::env::temp_dir(), u64::MAX);
        assert!(!decision.allowed);
        assert!(decision.available_bytes.is_some());
        assert_eq!(decision.reserve_bytes, u64::MAX);
        assert!(decision.reason.contains("insufficient disk"));
    }

    #[test]
    fn missing_path_whose_parent_exists_returns_decision_without_creating() {
        let parent = std::env::temp_dir();
        let missing: PathBuf = parent.join("nxr-disk-preflight-missing-path-does-not-exist");
        assert!(
            !missing.exists(),
            "precondition: missing path must not already exist"
        );
        let decision = disk_preflight(&missing, 1);
        assert!(
            !missing.exists(),
            "disk_preflight must not create the missing path"
        );
        assert!(
            decision.allowed || decision.available_bytes.is_some() || !decision.reason.is_empty(),
            "expected a decision for a missing path with an existing parent; got {decision:?}"
        );
        // Parent exists on a normal temp dir, so Unix should resolve via ancestor.
        #[cfg(unix)]
        {
            assert!(decision.available_bytes.is_some());
            assert!(decision.allowed);
            assert!(decision.reason.contains("enough disk"));
        }
    }
}
