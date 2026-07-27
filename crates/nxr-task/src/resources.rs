//! Resource reservations and exclusivity locks for cooperative scheduling.

use serde::{Deserialize, Serialize};

use crate::memory::parse_memory;
use crate::schema::TaskResources;

/// Normalized per-node resource demand carried in execution plans.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeResources {
    /// Soft CPU token demand (0 = default 1 token when scheduling).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub cpu: u32,
    /// Soft memory demand in bytes (0 = no reservation).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub memory_bytes: u64,
    /// Named mutex locks; at most one in-flight node may hold each name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusive: Vec<String>,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

impl NodeResources {
    /// No resource reservations or locks.
    pub const EMPTY: Self = Self {
        cpu: 0,
        memory_bytes: 0,
        exclusive: Vec::new(),
    };

    /// CPU tokens consumed by this node (defaults to 1 when unset).
    #[must_use]
    pub fn cpu_tokens(&self) -> u32 {
        if self.cpu == 0 { 1 } else { self.cpu }
    }

    /// Whether this node declares no resource reservations or locks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cpu == 0 && self.memory_bytes == 0 && self.exclusive.is_empty()
    }

    /// Build from a task `resources` block.
    #[must_use]
    pub fn from_task_resources(resources: &TaskResources) -> Self {
        let memory_bytes = resources
            .memory
            .as_deref()
            .and_then(|raw| parse_memory(raw).ok())
            .unwrap_or(0);
        Self {
            cpu: resources.cpu.unwrap_or(0),
            memory_bytes,
            exclusive: resources.exclusive.clone(),
        }
    }
}

/// Soft token pool limits for the scheduler (jobs remain the hard cap).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    /// Total CPU tokens available to in-flight nodes.
    pub cpu_pool: u32,
    /// Total memory bytes available to in-flight nodes (0 = unlimited).
    pub memory_pool: u64,
}

impl ResourceLimits {
    /// Derive limits from the job count and optional host memory ceiling.
    #[must_use]
    pub fn from_jobs(jobs: usize) -> Self {
        Self {
            cpu_pool: u32::try_from(jobs).unwrap_or(u32::MAX).max(1),
            memory_pool: memory_pool_from_env(),
        }
    }
}

fn memory_pool_from_env() -> u64 {
    std::env::var("NXR_MEMORY_POOL")
        .ok()
        .and_then(|raw| parse_memory(&raw).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{NodeResources, ResourceLimits};
    use crate::schema::TaskResources;

    #[test]
    fn cpu_tokens_default_to_one() {
        assert_eq!(NodeResources::EMPTY.cpu_tokens(), 1);
        assert_eq!(
            NodeResources {
                cpu: 4,
                ..NodeResources::EMPTY
            }
            .cpu_tokens(),
            4
        );
    }

    #[test]
    fn from_task_resources_parses_memory() {
        let resources = TaskResources {
            cpu: Some(2),
            memory: Some("512MiB".to_owned()),
            io: None,
            network: None,
            exclusive: vec!["cargo-target".to_owned()],
        };
        let node = NodeResources::from_task_resources(&resources);
        assert_eq!(node.cpu, 2);
        assert_eq!(node.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(node.exclusive, vec!["cargo-target".to_owned()]);
    }

    #[test]
    fn resource_limits_from_jobs() {
        let limits = ResourceLimits::from_jobs(4);
        assert_eq!(limits.cpu_pool, 4);
    }
}
